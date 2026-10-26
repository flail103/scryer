use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompletedDownloadPathState {
    Ready,
    Retry,
    Blocked,
}

pub(super) fn evaluate_completed_download_path(
    td: &mut TrackedDownload,
    completed: &CompletedDownload,
    now: DateTime<Utc>,
) -> CompletedDownloadPathState {
    if completed_download_path_is_unsupported_url(&completed.dest_dir) {
        td.path_missing_since = None;
        td.status = TrackedDownloadStatus::Warning;
        td.status_messages = vec![crate::tracked_downloads::TrackedStatusMessage::new(
            crate::tracked_downloads::TrackedStatusMessageKind::PathUnsupported,
            PATH_URL_UNSUPPORTED_MESSAGE,
        )];
        return CompletedDownloadPathState::Blocked;
    }

    let path_available =
        !completed.dest_dir.trim().is_empty() && Path::new(&completed.dest_dir).exists();

    if path_available {
        td.path_missing_since = None;
        clear_path_warnings(td);
        return CompletedDownloadPathState::Ready;
    }

    let missing_since = td.path_missing_since.get_or_insert(now);
    let elapsed = now.signed_duration_since(*missing_since);
    if elapsed < Duration::minutes(COMPLETED_PATH_GRACE_PERIOD_MINUTES) {
        td.status = TrackedDownloadStatus::Warning;
        td.status_messages = vec![crate::tracked_downloads::TrackedStatusMessage::new(
            crate::tracked_downloads::TrackedStatusMessageKind::PathWaiting,
            PATH_WAITING_MESSAGE,
        )];
        return CompletedDownloadPathState::Retry;
    }

    td.status = TrackedDownloadStatus::Warning;
    td.status_messages = vec![crate::tracked_downloads::TrackedStatusMessage::new(
        crate::tracked_downloads::TrackedStatusMessageKind::PathUnavailable,
        missing_completed_download_path_message(completed),
    )];
    CompletedDownloadPathState::Blocked
}

fn missing_completed_download_path_message(completed: &CompletedDownload) -> String {
    if completed.dest_dir.contains("/completed-symlinks/") {
        PATH_BLOCKED_NZBDAV_SYMLINK_MESSAGE.to_string()
    } else {
        PATH_BLOCKED_MESSAGE.to_string()
    }
}

fn completed_download_path_is_unsupported_url(dest_dir: &str) -> bool {
    let normalized = dest_dir.trim().to_ascii_lowercase();
    normalized.contains("://") || normalized.starts_with("webdav:")
}

fn clear_path_warnings(td: &mut TrackedDownload) {
    td.status_messages.retain(|message| {
        !matches!(
            message.kind,
            crate::tracked_downloads::TrackedStatusMessageKind::PathWaiting
                | crate::tracked_downloads::TrackedStatusMessageKind::PathUnavailable
                | crate::tracked_downloads::TrackedStatusMessageKind::PathUnsupported
        )
    });
    if td.status_messages.is_empty() && td.status == TrackedDownloadStatus::Warning {
        td.status = TrackedDownloadStatus::Ok;
    }
}
