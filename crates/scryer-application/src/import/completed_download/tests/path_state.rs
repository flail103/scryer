use super::*;

#[test]
fn completed_download_path_retries_during_grace_window_and_blocks_after_deadline() {
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
        CompletedDownloadPathState::Blocked
    );
    assert_eq!(
        crate::tracked_downloads::status_texts(&td.status_messages),
        vec![PATH_BLOCKED_MESSAGE.to_string()]
    );
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
