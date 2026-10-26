use super::*;

#[tokio::test]
async fn completed_download_rechecks_a_path_that_arrives_after_the_grace_window() {
    let directory = tempfile::tempdir().expect("temporary sync root");
    let late_path = directory.path().join("synced-download");
    let release = "Paper.Lantern.2012.1080p.WEB-DL";
    let completed = build_completed_download(release, &late_path.to_string_lossy(), Some("movie"));
    let app = build_app_with_download_client(
        vec![build_title("title-1", "Paper Lantern", MediaFacet::Movie)],
        vec![],
        vec![],
        vec![],
        test_download_client_with_completed(completed),
    );
    let mut tracked = build_tracked_download("title-1", "movie", release);
    tracked.path_missing_since =
        Some(Utc::now() - Duration::minutes(COMPLETED_PATH_GRACE_PERIOD_MINUTES + 1));

    check(&app, &mut tracked).await;
    assert_eq!(tracked.state, TrackedDownloadState::Downloading);
    assert!(tracked.import_retry_deferred(Utc::now()));
    std::fs::create_dir(&late_path).expect("sync directory arrives");
    std::fs::write(late_path.join(format!("{release}.mkv")), b"synthetic video")
        .expect("sync video arrives");

    check(&app, &mut tracked).await;
    assert_eq!(tracked.state, TrackedDownloadState::Downloading);
    tracked
        .import_execution_retry
        .as_mut()
        .expect("missing-path retry")
        .next_retry_at = Utc::now() - Duration::seconds(1);
    check(&app, &mut tracked).await;
    assert_eq!(tracked.state, TrackedDownloadState::ImportPending);
    assert!(tracked.path_missing_since.is_none());
}

#[test]
fn completed_download_path_keeps_retrying_after_the_grace_window() {
    let mut td = build_tracked_download("title-1", "movie", "Paper.Lantern.2012.1080p");
    let missing_dir = std::env::temp_dir().join(format!("scryer-missing-path-{}", Id::new().0));
    let completed = build_completed_download(
        "Paper.Lantern.2012.1080p",
        missing_dir.to_string_lossy().as_ref(),
        Some("movie"),
    );
    let now = Utc::now();

    assert_eq!(
        evaluate_completed_download_path(&mut td, &completed, now),
        CompletedDownloadPathState::Retry
    );
    assert_eq!(
        crate::tracked_downloads::status_texts(&td.status_messages),
        vec![PATH_WAITING_MESSAGE.to_string()]
    );
    assert_eq!(td.path_missing_since, Some(now));

    assert_eq!(
        evaluate_completed_download_path(
            &mut td,
            &completed,
            now + Duration::minutes(COMPLETED_PATH_GRACE_PERIOD_MINUTES + 1),
        ),
        CompletedDownloadPathState::Retry
    );
    assert_eq!(
        crate::tracked_downloads::status_texts(&td.status_messages),
        vec![PATH_BLOCKED_MESSAGE.to_string()]
    );
    assert!(
        td.import_retry_deferred(now + Duration::minutes(COMPLETED_PATH_GRACE_PERIOD_MINUTES + 1))
    );
}

#[tokio::test]
async fn completed_availability_retry_resumes_after_restart_without_reopening_manual_blocks() {
    let directory = tempfile::tempdir().expect("temporary sync root");
    let release = "Paper.Lantern.2012.1080p.WEB-DL";
    std::fs::write(
        directory.path().join(format!("{release}.mkv")),
        b"synthetic video",
    )
    .expect("synced video");
    let completed =
        build_completed_download(release, &directory.path().to_string_lossy(), Some("movie"));
    let submissions = Arc::new(TestDownloadSubmissionRepo::default());
    let app = build_app_with_download_client_configs_and_submissions(
        vec![build_title("title-1", "Paper Lantern", MediaFacet::Movie)],
        vec![],
        vec![],
        vec![],
        test_download_client_with_completed(completed),
        Arc::new(NullDownloadClientConfigRepository),
        submissions.clone(),
    );
    for persisted_state in ["downloading", "import_pending", "import_blocked"] {
        let original = build_tracked_download("title-1", "movie", release);
        submissions
            .record_identity_tracked_state_for_download(
                Some(&original.download_id),
                &DownloadSubmissionIdentity::default(),
                None,
                persisted_state,
                Some(crate::tracked_downloads::ImportBlockedReason::ManualMappingRequired.as_str()),
                None,
            )
            .await
            .expect("persist pre-restart state");
        let mut restored =
            crate::tracked_downloads::TrackedDownloadService::build_new_tracked_download(
                &app,
                original.download_id,
                original.id,
                original.client_item,
            )
            .await;
        check(&app, &mut restored).await;
        let expected = if persisted_state == "import_blocked" {
            TrackedDownloadState::ImportBlocked
        } else {
            TrackedDownloadState::ImportPending
        };
        assert_eq!(restored.state, expected, "{persisted_state}");
    }
}

#[test]
fn completed_download_path_ready_clears_waiting_state_when_path_appears() {
    let mut td = build_tracked_download("title-1", "movie", "Paper.Lantern.2012.1080p");
    td.path_missing_since = Some(Utc::now() - Duration::minutes(5));
    td.status = TrackedDownloadStatus::Warning;
    td.status_messages = vec![crate::tracked_downloads::TrackedStatusMessage::new(
        crate::tracked_downloads::TrackedStatusMessageKind::PathWaiting,
        "rewritten waiting explanation",
    )];

    let existing_dir = std::env::temp_dir().join(format!("scryer-path-ready-{}", Id::new().0));
    std::fs::create_dir_all(&existing_dir).expect("create temp dir");
    let completed = build_completed_download(
        "Paper.Lantern.2012.1080p",
        existing_dir.to_string_lossy().as_ref(),
        Some("movie"),
    );

    assert_eq!(
        evaluate_completed_download_path(&mut td, &completed, Utc::now()),
        CompletedDownloadPathState::Ready
    );
    assert!(td.path_missing_since.is_none());
    assert!(td.status_messages.is_empty());
    assert_eq!(td.status, TrackedDownloadStatus::Ok);

    std::fs::remove_dir_all(&existing_dir).expect("remove temp dir");
}

#[test]
fn completed_download_path_blocks_immediately_for_url_sources() {
    let mut td = build_tracked_download("title-1", "movie", "Paper.Lantern.2012.1080p");
    let completed = build_completed_download(
        "Paper.Lantern.2012.1080p",
        "https://nzbdav.example/remote/completed-symlinks/Paper.Lantern.2012.1080p",
        Some("movie"),
    );

    assert_eq!(
        evaluate_completed_download_path(&mut td, &completed, Utc::now()),
        CompletedDownloadPathState::Blocked
    );
    assert!(td.path_missing_since.is_none());
    assert_eq!(
        crate::tracked_downloads::status_texts(&td.status_messages),
        vec![PATH_URL_UNSUPPORTED_MESSAGE.to_string()]
    );
}
