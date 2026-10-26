use crate::{AppError, AppResult, BackupBundleInspectSummary, RestoreDeadline, restore_refusal};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Notify, OwnedRwLockReadGuard, RwLock};
use tokio_util::sync::CancellationToken;

pub const RESTORE_JOB_FILENAME: &str = "restore-job.json";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestoreJobKind {
    Inspect,
    Apply,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestoreJobPhase {
    Inspecting,
    Draining,
    Restoring,
    Cancelling,
    Restarting,
    Complete,
    Failed,
    Interrupted,
}
impl RestoreJobPhase {
    pub fn active(self) -> bool {
        matches!(
            self,
            Self::Inspecting
                | Self::Draining
                | Self::Restoring
                | Self::Cancelling
                | Self::Restarting
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoreJobStatus {
    pub id: String,
    pub upload_id: String,
    pub kind: RestoreJobKind,
    pub phase: RestoreJobPhase,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub reason: Option<String>,
    #[serde(default)]
    pub import_percent: Option<u8>,
}

/// One bounded slot; accepted work and application admission share this owner.
pub struct RestoreCoordinator {
    state: Mutex<Option<RestoreJobStatus>>,
    path: Mutex<Option<PathBuf>>,
    summary: Mutex<Option<BackupBundleInspectSummary>>,
    exclusive: AtomicBool,
    pub changed: Notify,
    requests: Arc<RwLock<()>>,
    pub shutdown_workers: CancellationToken,
    workers: tokio_util::task::TaskTracker,
    worker_failed: Arc<AtomicBool>,
    deadline: Mutex<Option<RestoreDeadline>>,
    worker_running: AtomicBool,
    shutting_down: AtomicBool,
}
impl Default for RestoreCoordinator {
    fn default() -> Self {
        Self {
            state: Mutex::new(None),
            path: Mutex::new(None),
            summary: Mutex::new(None),
            exclusive: AtomicBool::new(false),
            changed: Notify::new(),
            requests: Arc::new(RwLock::new(())),
            shutdown_workers: CancellationToken::new(),
            workers: tokio_util::task::TaskTracker::new(),
            worker_failed: Arc::new(AtomicBool::new(false)),
            deadline: Mutex::new(None),
            worker_running: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
        }
    }
}
impl RestoreCoordinator {
    /// Reconstruct ownership before opening the application datastore.
    pub fn recover(data_dir: &Path) -> AppResult<Self> {
        let coordinator = Self::default();
        if let Some(mut status) = read_restore_job(data_dir)? {
            if status.phase.active() && status.phase != RestoreJobPhase::Restarting {
                status.phase = RestoreJobPhase::Interrupted;
                status.reason = Some(restore_refusal::RESTORE_INTERRUPTED.to_owned());
                persist_status(&data_dir.join(RESTORE_JOB_FILENAME), &status)?;
            }
            coordinator.exclusive.store(
                status.kind == RestoreJobKind::Apply && status.phase != RestoreJobPhase::Complete,
                Ordering::Release,
            );
            coordinator.worker_running.store(
                status.phase == RestoreJobPhase::Restarting,
                Ordering::Release,
            );
            *coordinator.path.lock().expect("restore path lock") =
                Some(data_dir.join(RESTORE_JOB_FILENAME));
            *coordinator.state.lock().expect("restore status lock") = Some(status);
        }
        Ok(coordinator)
    }

    /// Only the host calls this after promotion, migrations and initialization.
    pub fn finish_startup(&self) -> AppResult<()> {
        if let Some(status) = self
            .snapshot()
            .filter(|s| s.phase == RestoreJobPhase::Restarting)
        {
            if let Some(deadline) = self
                .deadline
                .lock()
                .expect("restore deadline lock")
                .as_ref()
            {
                deadline.check()?;
            }
            self.transition(&status.id, RestoreJobPhase::Complete, None)?;
            self.exclusive.store(false, Ordering::Release);
            self.changed.notify_waiters();
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Option<RestoreJobStatus> {
        let mut status = self.state.lock().expect("restore status lock").clone()?;
        if let Some(deadline) = self
            .deadline
            .lock()
            .expect("restore deadline lock")
            .as_ref()
        {
            status.import_percent = deadline.import_percent().or(status.import_percent);
        }
        Some(status)
    }
    pub fn can_retry(&self) -> bool {
        !self.worker_running.load(Ordering::Acquire)
            && !self.shutting_down.load(Ordering::Acquire)
            && self.snapshot().is_some_and(|status| {
                matches!(
                    status.phase,
                    RestoreJobPhase::Failed | RestoreJobPhase::Interrupted
                ) && status.reason.as_deref() != Some(restore_refusal::RESTORE_RECOVERY_REQUIRED)
            })
    }

    pub fn track_deadline(&self, deadline: &RestoreDeadline) {
        *self.deadline.lock().expect("restore deadline lock") = Some(deadline.clone());
        if self.shutting_down.load(Ordering::Acquire) {
            deadline.cancel();
        }
    }
    /// Keep the listener alive while accepted work cancels and rolls back.
    pub async fn drain_for_shutdown(&self) {
        {
            let _state = self.state.lock().expect("restore status lock");
            self.shutting_down.store(true, Ordering::Release);
        }
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(deadline) = self
                .deadline
                .lock()
                .expect("restore deadline lock")
                .as_ref()
            {
                deadline.cancel();
            }
            if !self.worker_running.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    pub fn blocks_application(&self) -> bool {
        self.exclusive.load(Ordering::Acquire)
    }
    pub fn summary(&self) -> Option<BackupBundleInspectSummary> {
        self.summary.lock().expect("restore summary lock").clone()
    }
    pub fn set_summary(&self, value: BackupBundleInspectSummary) {
        *self.summary.lock().expect("restore summary lock") = Some(value);
    }
    pub fn register_worker(&self, worker: tokio::task::JoinHandle<()>) {
        let failed = self.worker_failed.clone();
        self.workers.spawn(async move {
            if worker.await.is_err() {
                failed.store(true, Ordering::Release);
            }
        });
    }

    /// Track detached work as well as the poller or request which starts it.
    pub fn spawn_task<F>(&self, work: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        struct CompletionGuard {
            failed: Arc<AtomicBool>,
            completed: bool,
        }
        impl Drop for CompletionGuard {
            fn drop(&mut self) {
                if !self.completed {
                    self.failed.store(true, Ordering::Release);
                }
            }
        }
        // Construct before spawning so cancellation before the first poll also
        // records an unclean exit.
        let mut guard = CompletionGuard {
            failed: self.worker_failed.clone(),
            completed: false,
        };
        self.workers.spawn(async move {
            let result = work.await;
            guard.completed = true;
            drop(guard);
            result
        })
    }

    pub fn spawn_worker(
        self: &Arc<Self>,
        work: impl std::future::Future<Output = ()> + Send + 'static,
    ) {
        let owner = self.clone();
        self.register_worker(tokio::spawn(async move {
            loop {
                let changed = owner.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if owner.shutdown_workers.is_cancelled() {
                    return;
                }
                if !owner.blocks_application() {
                    break;
                }
                tokio::select! {
                    _ = changed => {},
                    _ = owner.shutdown_workers.cancelled() => return,
                }
            }
            work.await;
        }));
    }

    pub async fn admit_request(&self) -> AppResult<OwnedRwLockReadGuard<()>> {
        if self.blocks_application() {
            return Err(Self::maintenance_error());
        }
        let guard = self.requests.clone().read_owned().await;
        if self.blocks_application() {
            return Err(Self::maintenance_error());
        }
        Ok(guard)
    }
    pub fn maintenance_error() -> AppError {
        AppError::validation_refused(
            restore_refusal::RESTORE_MAINTENANCE_ACTIVE,
            "The application is unavailable while restore or recovery is in progress",
        )
    }

    pub fn start(
        &self,
        data_dir: &Path,
        upload_id: &str,
        kind: RestoreJobKind,
    ) -> AppResult<(RestoreJobStatus, bool)> {
        let mut state = self.state.lock().expect("restore status lock");
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(Self::maintenance_error());
        }
        if state.as_ref().is_some_and(|status| {
            status.reason.as_deref() == Some(restore_refusal::RESTORE_RECOVERY_REQUIRED)
        }) {
            return Err(AppError::validation_refused(
                restore_refusal::RESTORE_RECOVERY_REQUIRED,
                "Recovery must be completed before another restore can start",
            ));
        }
        if let Some(active) = state.as_ref().filter(|s| s.phase.active()) {
            return if active.upload_id == upload_id && active.kind == kind {
                Ok((active.clone(), false))
            } else {
                Err(Self::maintenance_error())
            };
        }
        let started_at = Utc::now();
        let status = RestoreJobStatus {
            id: uuid::Uuid::new_v4().to_string(),
            upload_id: upload_id.to_owned(),
            kind,
            phase: if kind == RestoreJobKind::Inspect {
                RestoreJobPhase::Inspecting
            } else {
                RestoreJobPhase::Draining
            },
            started_at,
            expires_at: started_at + chrono::Duration::hours(1),
            reason: None,
            import_percent: None,
        };
        let path = data_dir.join(RESTORE_JOB_FILENAME);
        persist_status(&path, &status)?;
        *self.path.lock().expect("restore path lock") = Some(path);
        if kind == RestoreJobKind::Inspect
            || state.as_ref().is_none_or(|s| s.upload_id != upload_id)
        {
            *self.summary.lock().expect("restore summary lock") = None;
        }
        *state = Some(status.clone());
        self.worker_running.store(true, Ordering::Release);
        *self.deadline.lock().expect("restore deadline lock") = None;
        if kind == RestoreJobKind::Apply {
            self.exclusive.store(true, Ordering::Release);
        }
        self.changed.notify_waiters();
        Ok((status, true))
    }

    pub fn transition(
        &self,
        id: &str,
        phase: RestoreJobPhase,
        reason: Option<&str>,
    ) -> AppResult<()> {
        let mut state = self.state.lock().expect("restore status lock");
        let current = state.as_mut().filter(|s| s.id == id).ok_or_else(|| {
            AppError::validation_refused(
                restore_refusal::RESTORE_ATTEMPT_NOT_FOUND,
                "Restore attempt does not exist",
            )
        })?;
        let mut next = current.clone();
        next.phase = phase;
        next.reason = reason.map(str::to_owned);
        if let Some(deadline) = self
            .deadline
            .lock()
            .expect("restore deadline lock")
            .as_ref()
        {
            next.import_percent = deadline.import_percent().or(next.import_percent);
        }
        let persisted = match self.path.lock().expect("restore path lock").as_ref() {
            Some(path) => persist_status(path, &next),
            None => Ok(()),
        };
        *current = next;
        if matches!(
            phase,
            RestoreJobPhase::Complete
                | RestoreJobPhase::Failed
                | RestoreJobPhase::Interrupted
                | RestoreJobPhase::Restarting
        ) {
            self.worker_running.store(false, Ordering::Release);
        }
        self.changed.notify_waiters();
        persisted
    }

    /// A failed bootstrap cannot certify rollback or a usable restored datastore.
    /// Keep access closed, but record that the startup worker has terminated.
    pub fn fail_startup(&self) {
        if self.blocks_application()
            && let Some(status) = self.snapshot()
            && let Err(error) = self.transition(
                &status.id,
                RestoreJobPhase::Failed,
                Some(restore_refusal::RESTORE_RECOVERY_REQUIRED),
            )
        {
            tracing::error!(%error, "could not persist restore recovery status");
        }
    }

    pub fn fail(&self, id: &str, deadline: &RestoreDeadline, error: AppError) {
        tracing::error!(attempt_id = id, error = %error, "restore worker failed");
        let reason =
            if error.validation_reason() == Some(restore_refusal::RESTORE_RECOVERY_REQUIRED) {
                restore_refusal::RESTORE_RECOVERY_REQUIRED
            } else if deadline.check().is_err() {
                restore_refusal::RESTORE_DEADLINE_EXCEEDED
            } else {
                error
                    .validation_reason()
                    .unwrap_or(restore_refusal::RESTORE_INTERRUPTED)
            };
        if let Err(error) = self.transition(id, RestoreJobPhase::Failed, Some(reason)) {
            tracing::error!(attempt_id = id, error = %error, "could not persist failed restore status");
        }
    }

    /// Expiry requests cancellation but never drops accepted blocking work.
    pub async fn supervise<T>(
        &self,
        id: &str,
        deadline: &RestoreDeadline,
        work: impl std::future::Future<Output = AppResult<T>>,
    ) -> AppResult<T> {
        self.track_deadline(deadline);
        tokio::pin!(work);
        tokio::select! {
            biased;
            _ = deadline.expired() => {
                let transition = self.transition(id, RestoreJobPhase::Cancelling, Some(restore_refusal::RESTORE_DEADLINE_EXCEEDED));
                if let Err(error) = work.await
                    && error.validation_reason() == Some(restore_refusal::RESTORE_RECOVERY_REQUIRED) {
                    return Err(error);
                }
                transition?;
                Err(RestoreDeadline::expired_error())
            },
            result = &mut work => result,
        }
    }

    /// Never abort task handles: their return proves their datastore work drained.
    pub async fn drain(&self, deadline: &RestoreDeadline) -> AppResult<()> {
        self.shutdown_workers.cancel();
        let _drained = self.requests.write().await;
        self.workers.close();
        self.workers.wait().await;
        if self.worker_failed.load(Ordering::Acquire) {
            return Err(AppError::validation_refused(
                restore_refusal::RESTORE_INTERRUPTED,
                "A background worker failed while draining; restore cannot safely proceed",
            ));
        }
        deadline.check()
    }
}

pub fn read_restore_job(data_dir: &Path) -> AppResult<Option<RestoreJobStatus>> {
    let path = data_dir.join(RESTORE_JOB_FILENAME);
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| AppError::Repository(format!("invalid restore status record: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::Repository(format!(
            "could not read restore status: {e}"
        ))),
    }
}

fn persist_status(path: &Path, status: &RestoreJobStatus) -> AppResult<()> {
    use std::io::Write;
    let directory = path.parent().expect("restore status parent");
    let mut file = tempfile::NamedTempFile::new_in(directory)
        .map_err(|e| AppError::Repository(format!("could not create restore status: {e}")))?;
    #[cfg(windows)]
    protect_restore_record(file.path())
        .map_err(|e| AppError::Repository(format!("could not protect restore status: {e}")))?;
    serde_json::to_writer(&mut file, status)
        .map_err(|e| AppError::Repository(format!("could not encode restore status: {e}")))?;
    file.flush()
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| AppError::Repository(format!("could not flush restore status: {e}")))?;
    file.persist(path).map_err(|e| {
        AppError::Repository(format!("could not persist restore status: {}", e.error))
    })?;
    Ok(())
}

#[cfg(windows)]
fn protect_restore_record(path: &Path) -> std::io::Result<()> {
    use std::os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    };
    use windows_sys::Win32::{
        Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, DACL_SECURITY_INFORMATION,
            GetLengthSid, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
            PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_DESCRIPTOR, SetFileSecurityW,
            SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::FILE_ALL_ACCESS,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    fn checked(result: i32) -> std::io::Result<()> {
        if result == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
    let mut token = std::ptr::null_mut();
    // SAFETY: Windows initializes this out-handle; OwnedHandle closes it on every exit.
    unsafe {
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut token,
        ))?;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut required = 0;
    // SAFETY: A null buffer asks Windows for the required size only.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut required,
        );
    }
    if required < std::mem::size_of::<TOKEN_USER>() as u32 {
        return Err(std::io::Error::last_os_error());
    }
    let mut user = vec![0_usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: The aligned buffer holds `required` bytes; the SID remains valid
    // until AddAccessAllowedAce copies it into our independently owned ACL.
    unsafe {
        checked(GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            user.as_mut_ptr().cast(),
            required,
            &mut required,
        ))?;
        let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        let acl_size = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            - std::mem::size_of::<u32>()
            + GetLengthSid(sid) as usize;
        let mut acl = vec![0_u32; acl_size.div_ceil(std::mem::size_of::<u32>())];
        let acl_ptr = acl.as_mut_ptr().cast::<ACL>();
        checked(InitializeAcl(acl_ptr, acl_size as u32, ACL_REVISION))?;
        checked(AddAccessAllowedAce(
            acl_ptr,
            ACL_REVISION,
            FILE_ALL_ACCESS,
            sid,
        ))?;
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let descriptor_ptr = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
        checked(InitializeSecurityDescriptor(descriptor_ptr, 1))?;
        checked(SetSecurityDescriptorDacl(descriptor_ptr, 1, acl_ptr, 0))?;
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        checked(SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor_ptr,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn background_restore_refuses_after_child_task_panic() {
        let coordinator = RestoreCoordinator::default();
        let task = coordinator.spawn_task(async { panic!("synthetic worker failure") });
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(60), task)
                .await
                .unwrap()
                .unwrap_err()
                .is_panic()
        );
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            coordinator.drain(&RestoreDeadline::new()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.validation_reason(),
            Some(restore_refusal::RESTORE_INTERRUPTED)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn background_restore_drains_children_registered_after_drain_starts() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = Arc::new(RestoreCoordinator::default());
        let (release_parent, parent_ready) = tokio::sync::oneshot::channel();
        let (release_child, child_ready) = tokio::sync::oneshot::channel();
        let (child_started, started) = tokio::sync::oneshot::channel();
        let parent = coordinator.clone();
        coordinator.spawn_task(async move {
            parent_ready.await.unwrap();
            parent.spawn_task(async move {
                child_ready.await.unwrap();
            });
            child_started.send(()).unwrap();
        });
        coordinator
            .start(directory.path(), "fixture", RestoreJobKind::Apply)
            .unwrap();
        let deadline = RestoreDeadline::new();
        let drain = coordinator.drain(&deadline);
        tokio::pin!(drain);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(drain.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        release_parent.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(60), started)
            .await
            .unwrap()
            .unwrap();
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(drain.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(coordinator.admit_request().await.is_err());
        release_child.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(60), drain)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn background_restore_failed_startup_releases_worker_but_keeps_recovery_guard() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = RestoreCoordinator::default();
        let (job, _) = coordinator
            .start(directory.path(), "fixture", RestoreJobKind::Apply)
            .unwrap();
        coordinator
            .transition(&job.id, RestoreJobPhase::Restarting, None)
            .unwrap();
        let recovered = RestoreCoordinator::recover(directory.path()).unwrap();
        recovered.fail_startup();
        assert!(recovered.blocks_application());
        assert!(!recovered.can_retry());
        assert!(
            recovered
                .start(directory.path(), "fixture", RestoreJobKind::Apply)
                .is_err()
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            recovered.drain_for_shutdown(),
        )
        .await
        .unwrap();
        assert_eq!(
            read_restore_job(directory.path())
                .unwrap()
                .unwrap()
                .reason
                .as_deref(),
            Some(restore_refusal::RESTORE_RECOVERY_REQUIRED)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn background_restore_expiry_keeps_ownership_until_worker_exit() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = Arc::new(RestoreCoordinator::default());
        let lease = coordinator.admit_request().await.unwrap();
        let (status, fresh) = coordinator
            .start(directory.path(), "fixture", RestoreJobKind::Apply)
            .unwrap();
        assert!(fresh);
        assert!(coordinator.admit_request().await.is_err());
        let (exiting, exited) = tokio::sync::oneshot::channel();
        let cancellation = coordinator.shutdown_workers.clone();
        coordinator.register_worker(tokio::spawn(async move {
            cancellation.cancelled().await;
            exited.await.unwrap();
        }));
        let deadline = RestoreDeadline::new();
        let drain = coordinator.drain(&deadline);
        tokio::pin!(drain);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(drain.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::advance(crate::RESTORE_WORK_BUDGET).await;
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(drain.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(coordinator.blocks_application());
        exiting.send(()).unwrap();
        drop(lease);
        assert_eq!(
            drain.await.unwrap_err().validation_reason(),
            Some(restore_refusal::RESTORE_DEADLINE_EXCEEDED)
        );
        coordinator
            .transition(
                &status.id,
                RestoreJobPhase::Failed,
                Some(restore_refusal::RESTORE_DEADLINE_EXCEEDED),
            )
            .unwrap();
        assert!(coordinator.blocks_application());
        assert_eq!(
            read_restore_job(directory.path()).unwrap().unwrap().phase,
            RestoreJobPhase::Failed
        );
    }

    #[test]
    fn background_restore_duplicate_start_retains_attempt_and_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = RestoreCoordinator::default();
        let (original, _) = coordinator
            .start(directory.path(), "fixture", RestoreJobKind::Apply)
            .unwrap();
        let (duplicate, fresh) = coordinator
            .start(directory.path(), "fixture", RestoreJobKind::Apply)
            .unwrap();
        assert!(!fresh);
        assert_eq!(original.id, duplicate.id);
        assert_eq!(original.expires_at, duplicate.expires_at);
        assert!(
            coordinator
                .start(directory.path(), "other", RestoreJobKind::Apply)
                .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn background_restore_supervisor_waits_for_cancelled_worker() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = RestoreCoordinator::default();
        let (job, _) = coordinator
            .start(directory.path(), "fixture", RestoreJobKind::Apply)
            .unwrap();
        let deadline = RestoreDeadline::new();
        let (release, released) = tokio::sync::oneshot::channel();
        let work = async {
            released.await.unwrap();
            Ok::<_, AppError>(())
        };
        let supervised = coordinator.supervise(&job.id, &deadline, work);
        tokio::pin!(supervised);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(supervised.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::advance(crate::RESTORE_WORK_BUDGET).await;
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(supervised.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(
            coordinator.snapshot().unwrap().phase,
            RestoreJobPhase::Cancelling
        );
        assert!(coordinator.blocks_application());
        assert!(
            coordinator
                .start(directory.path(), "other", RestoreJobKind::Apply)
                .is_err()
        );
        release.send(()).unwrap();
        assert_eq!(
            supervised.await.unwrap_err().validation_reason(),
            Some(restore_refusal::RESTORE_DEADLINE_EXCEEDED)
        );
    }

    #[test]
    fn background_restore_restart_retains_deadline_and_interrupts_unhanded_work() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = RestoreCoordinator::default();
        let (job, _) = coordinator
            .start(directory.path(), "fixture", RestoreJobKind::Apply)
            .unwrap();
        let recovered = RestoreCoordinator::recover(directory.path()).unwrap();
        assert_eq!(
            recovered.snapshot().unwrap().phase,
            RestoreJobPhase::Interrupted
        );
        assert!(recovered.blocks_application());
        coordinator
            .transition(&job.id, RestoreJobPhase::Restarting, None)
            .unwrap();
        let recovered = RestoreCoordinator::recover(directory.path()).unwrap();
        assert_eq!(recovered.snapshot().unwrap().expires_at, job.expires_at);
        assert!(recovered.blocks_application());
        recovered.finish_startup().unwrap();
        assert!(!recovered.blocks_application());
        assert_eq!(
            read_restore_job(directory.path()).unwrap().unwrap().phase,
            RestoreJobPhase::Complete
        );
    }
}
