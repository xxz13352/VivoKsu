use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nwflash_infrastructure::{RemoteAssetDownloader, RemoteAssetSpec};
use tokio_util::sync::CancellationToken;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

fn temporary_directory() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after UNIX epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("nwflash-resource-downloader-{suffix}"))
}

#[tokio::test]
async fn falls_back_when_the_first_candidate_times_out() {
    let first_candidate = MockServer::start().await;
    let fallback_candidate = MockServer::start().await;

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(80)))
        .mount(&first_candidate)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"good"))
        .mount(&fallback_candidate)
        .await;

    let destination_root = temporary_directory();
    let destination = destination_root.join("fixture.bin");
    let downloader = RemoteAssetDownloader::new(
        None,
        Some(vec![fallback_candidate.uri()]),
        Some(Duration::from_secs(1)),
        Some(Duration::from_millis(20)),
    );
    let spec = RemoteAssetSpec::new("fixture", first_candidate.uri()).with_expected_length(4);

    let result = downloader
        .download_to_file(&spec, &destination, None, &CancellationToken::new())
        .await;

    assert_eq!(
        result.expect("the healthy fallback should finish the download"),
        4
    );
    assert_eq!(
        fs::read(&destination).expect("fallback output should be committed"),
        b"good"
    );
    fs::remove_dir_all(destination_root).expect("test fixture directory should be removable");
}
