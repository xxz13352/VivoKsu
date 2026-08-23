use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nwflash_infrastructure::{
    CloudflareClient, RemoteAssetDownloader, RemoteAssetSpec, ResourceDownloadError,
    DEFAULT_APP_VERSION,
};
use tokio_util::sync::CancellationToken;
use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be available")
        .as_nanos();
    std::env::temp_dir().join(format!("nwflash-e2e-{label}-{nonce}"))
}

#[tokio::test]
async fn cloudflare_authorization_503_is_reported_as_an_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/operation/authorize"))
        .and(header("Authorization", "Bearer session-token"))
        .respond_with(ResponseTemplate::new(503).set_body_string("authorization unavailable"))
        .expect(1)
        .mount(&server)
        .await;

    let client = CloudflareClient::new_injected(server.uri(), DEFAULT_APP_VERSION);
    let error = client
        .authorize_operation("session-token", "Flashing", "刷写 boot")
        .await
        .expect_err("a failed authorization service must not allow flashing");

    assert_eq!(error.status_code(), Some(503));
}

#[tokio::test]
async fn failed_github_and_download_candidates_preserve_the_approved_asset() {
    let github = MockServer::start().await;
    let download_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string("github unavailable"))
        .expect(1)
        .mount(&github)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"unverified replacement"))
        .expect(1)
        .mount(&download_server)
        .await;

    let root = temporary_directory("download-failure");
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let destination = root.join("approved-tool.exe");
    fs::write(&destination, b"approved").expect("approved fixture should be written");
    let downloader = RemoteAssetDownloader::new(
        None,
        Some(vec![download_server.uri()]),
        Some(Duration::from_secs(1)),
        Some(Duration::from_secs(1)),
    );
    let spec = RemoteAssetSpec::new("approved tool", github.uri()).with_expected_length(8);

    let error = downloader
        .download_to_file(&spec, &destination, None, &CancellationToken::new())
        .await
        .expect_err("all failed or unverified candidates must be rejected");

    assert!(matches!(
        error,
        ResourceDownloadError::AllCandidatesFailed { .. }
    ));
    assert_eq!(
        fs::read(&destination).expect("approved asset should remain readable"),
        b"approved"
    );
    assert_eq!(
        fs::read_dir(&root)
            .expect("fixture directory should be readable")
            .count(),
        1,
        "candidate staging files should be cleaned"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn cancellation_after_download_progress_removes_partial_staging() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0x5a; 128 * 1024]))
        .expect(1)
        .mount(&server)
        .await;

    let root = temporary_directory("download-cancellation");
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let destination = root.join("cancelled-tool.exe");
    let cancellation = CancellationToken::new();
    let cancel_after_progress = cancellation.clone();
    let progress = move |_| cancel_after_progress.cancel();
    let downloader = RemoteAssetDownloader::new(
        None,
        Some(Vec::new()),
        Some(Duration::from_secs(1)),
        Some(Duration::from_secs(1)),
    );
    let spec = RemoteAssetSpec::new("cancelled tool", server.uri());

    let error = downloader
        .download_to_file(&spec, &destination, Some(&progress), &cancellation)
        .await
        .expect_err("cancellation after the first chunk should stop the download");

    assert!(matches!(error, ResourceDownloadError::Cancelled));
    assert!(!destination.exists());
    assert_eq!(
        fs::read_dir(&root)
            .expect("fixture directory should be readable")
            .count(),
        0,
        "canceled staging files should be removed"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}
