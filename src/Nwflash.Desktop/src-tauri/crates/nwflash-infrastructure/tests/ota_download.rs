use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_infrastructure::{
    plan_ota_download, staging_download_path, validate_available_space, OtaDiskSpaceProvider,
    OtaDownloadError, OtaDownloadPlan, OtaDownloadPlanningError, OtaDownloadProgress,
    OtaDownloader, OTA_DOWNLOAD_MEMORY_CAP_BYTES,
};
use reqwest::Client;
use tokio_util::sync::CancellationToken;
use wiremock::{
    matchers::{header, method},
    Mock, MockServer, ResponseTemplate,
};

#[derive(Clone)]
struct FixedDiskSpace(u64);

impl OtaDiskSpaceProvider for FixedDiskSpace {
    fn available_bytes(&self, _destination: &Path) -> Result<u64, String> {
        Ok(self.0)
    }
}

static TEMPORARY_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temporary_directory() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after UNIX epoch")
        .as_nanos();
    temporary_directory_path(
        std::process::id(),
        suffix,
        TEMPORARY_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed),
    )
}

fn temporary_directory_path(process_id: u32, clock_tick: u128, sequence: u64) -> PathBuf {
    std::env::temp_dir().join(format!(
        "nwflash-ota-download-{process_id}-{clock_tick}-{sequence}"
    ))
}

#[test]
fn temporary_directory_identity_distinguishes_same_clock_tick() {
    assert_ne!(
        temporary_directory_path(42, 123, 1),
        temporary_directory_path(42, 123, 2),
        "parallel tests must not share a fixture directory when the system clock value matches"
    );
}

#[test]
fn range_server_uses_the_entire_known_file_range_with_bounded_memory() {
    let plan = plan_ota_download(Some(8 * 1024 * 1024), true, 8)
        .expect("known Range response should be plannable");

    assert_eq!(
        plan,
        OtaDownloadPlan::RangeParallel {
            range_start: 0,
            range_end: 8 * 1024 * 1024 - 1,
            connections: 8,
            memory_cap_bytes: OTA_DOWNLOAD_MEMORY_CAP_BYTES,
        }
    );
}

#[test]
fn staging_path_is_private_and_never_aliases_the_destination() {
    let destination = std::path::Path::new("C:\\ota\\firmware.zip");
    let staging =
        staging_download_path(destination, 42).expect("destination should produce staging path");

    assert_ne!(staging, destination);
    assert!(staging
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("partial-42"));
}

#[test]
fn non_range_server_falls_back_to_a_single_connection() {
    let plan = plan_ota_download(Some(1024), false, 8)
        .expect("known non-Range response should be plannable");

    assert_eq!(
        plan,
        OtaDownloadPlan::SingleConnection {
            content_length: 1024,
            memory_cap_bytes: OTA_DOWNLOAD_MEMORY_CAP_BYTES,
        }
    );
}

#[test]
fn unknown_or_zero_length_response_is_rejected_before_download() {
    assert!(matches!(
        plan_ota_download(None, true, 8),
        Err(OtaDownloadPlanningError::UnknownContentLength)
    ));
    assert!(matches!(
        plan_ota_download(Some(0), true, 8),
        Err(OtaDownloadPlanningError::UnknownContentLength)
    ));
}

#[test]
fn insufficient_disk_space_is_rejected_before_creating_a_download() {
    assert!(matches!(
        validate_available_space(1024, 1023),
        Err(OtaDownloadPlanningError::InsufficientDiskSpace { .. })
    ));
    assert!(validate_available_space(1024, 1024).is_ok());
}

#[tokio::test]
async fn range_probe_downloads_the_full_file_to_staging_before_replacing_destination() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "5")
                .insert_header("accept-ranges", "bytes"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-4"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", "5")
                .insert_header("content-range", "bytes 0-4/5")
                .set_body_bytes(b"fresh"),
        )
        .mount(&server)
        .await;

    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), 9);
    let reports = Arc::new(std::sync::Mutex::new(Vec::<OtaDownloadProgress>::new()));
    let progress_reports = Arc::clone(&reports);
    let progress = move |report| {
        progress_reports.lock().expect("progress lock").push(report);
    };

    let downloaded = downloader
        .download_to_file(
            &server.uri(),
            &destination,
            8,
            &CancellationToken::new(),
            Some(Arc::new(progress)),
        )
        .await
        .expect("range response should download atomically");

    assert_eq!(downloaded, 5);
    assert_eq!(
        fs::read(&destination).expect("final artifact should exist"),
        b"fresh"
    );
    assert!(
        reports
            .lock()
            .expect("progress lock")
            .last()
            .is_some_and(|progress| progress.downloaded_bytes == 5 && progress.total_bytes == 5),
        "the final 100% progress report must not be throttled away"
    );
    assert!(
        !directory.join(".firmware.zip.partial-9").exists(),
        "successful publication must remove the private staging file"
    );
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn range_probe_falls_back_when_head_is_not_allowed() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-0"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", "1")
                .insert_header("content-range", "bytes 0-0/5")
                .insert_header("accept-ranges", "bytes")
                .set_body_bytes(b"f"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-4"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", "5")
                .insert_header("content-range", "bytes 0-4/5")
                .set_body_bytes(b"fresh"),
        )
        .mount(&server)
        .await;

    let directory = temporary_directory();
    let destination = directory.join("firmware.zip");
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), 10);

    downloader
        .download_to_file(
            &server.uri(),
            &destination,
            8,
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("a range probe should recover from a rejected HEAD request");

    assert_eq!(
        fs::read(&destination).expect("final artifact should exist"),
        b"fresh"
    );
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn large_range_download_uses_bounded_parallel_segments() {
    const HALF: usize = 1024 * 1024;
    const TOTAL: usize = HALF * 2;
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", TOTAL.to_string())
                .insert_header("accept-ranges", "bytes"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("range", format!("bytes=0-{}", HALF - 1)))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", HALF.to_string())
                .insert_header("content-range", format!("bytes 0-{}/{}", HALF - 1, TOTAL))
                .set_body_bytes(vec![b'a'; HALF]),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("range", format!("bytes={HALF}-{}", TOTAL - 1)))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", HALF.to_string())
                .insert_header(
                    "content-range",
                    format!("bytes {HALF}-{}/{}", TOTAL - 1, TOTAL),
                )
                .set_body_bytes(vec![b'b'; HALF]),
        )
        .mount(&server)
        .await;

    let directory = temporary_directory();
    let destination = directory.join("firmware.zip");
    let downloader = OtaDownloader::new(
        Client::new(),
        Arc::new(FixedDiskSpace((TOTAL * 2) as u64)),
        11,
    );

    let downloaded = downloader
        .download_to_file(
            &server.uri(),
            &destination,
            2,
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("large range content should be retrieved by its two explicit segments");

    assert_eq!(downloaded, TOTAL as u64);
    let content = fs::read(&destination).expect("final artifact should exist");
    assert_eq!(content.len(), TOTAL);
    assert!(content[..HALF].iter().all(|byte| *byte == b'a'));
    assert!(content[HALF..].iter().all(|byte| *byte == b'b'));
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn malformed_single_range_response_preserves_existing_destination() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "5")
                .insert_header("accept-ranges", "bytes"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header("range", "bytes=0-4"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", "5")
                .insert_header("content-range", "bytes 0-3/5")
                .set_body_bytes(b"fresh"),
        )
        .mount(&server)
        .await;

    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), 12);

    assert!(
        downloader
            .download_to_file(
                &server.uri(),
                &destination,
                8,
                &CancellationToken::new(),
                None,
            )
            .await
            .is_err(),
        "a Range response with another byte boundary must be rejected"
    );
    assert_eq!(
        fs::read(&destination).expect("old completed artifact must remain"),
        b"old"
    );
    assert!(
        !directory.join(".firmware.zip.partial-12").exists(),
        "failed downloads must remove only their private partial"
    );
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn insufficient_disk_space_rejects_before_staging_or_destination_mutation() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "5"))
        .mount(&server)
        .await;
    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(4)), 13);

    assert!(
        downloader
            .download_to_file(
                &server.uri(),
                &destination,
                8,
                &CancellationToken::new(),
                None,
            )
            .await
            .is_err(),
        "insufficient capacity must fail before the GET starts"
    );
    assert_eq!(
        fs::read(&destination).expect("old output must remain"),
        b"old"
    );
    assert!(!directory.join(".firmware.zip.partial-13").exists());
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn unknown_content_length_rejects_before_destination_mutation() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), 14);

    assert!(
        downloader
            .download_to_file(
                &server.uri(),
                &destination,
                8,
                &CancellationToken::new(),
                None,
            )
            .await
            .is_err(),
        "downloads without a known length must never create a partial"
    );
    assert_eq!(
        fs::read(&destination).expect("old output must remain"),
        b"old"
    );
    assert!(!directory.join(".firmware.zip.partial-14").exists());
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn cancelled_before_download_preserves_existing_destination() {
    let server = MockServer::start().await;
    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), 15);

    assert!(
        downloader
            .download_to_file(&server.uri(), &destination, 8, &cancellation, None)
            .await
            .is_err(),
        "cancellation must short-circuit before probing or staging"
    );
    assert_eq!(
        fs::read(&destination).expect("old output must remain"),
        b"old"
    );
    assert!(!directory.join(".firmware.zip.partial-15").exists());
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn oversized_single_connection_body_preserves_existing_destination_and_removes_staging() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "5"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"too-long"))
        .mount(&server)
        .await;

    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let nonce = 16;
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), nonce);

    let error = downloader
        .download_to_file(
            &server.uri(),
            &destination,
            1,
            &CancellationToken::new(),
            None,
        )
        .await
        .expect_err("body larger than the probed length must fail before excess bytes are written");

    assert!(error.to_string().contains("超过"));
    assert_eq!(
        fs::read(&destination).expect("approved destination"),
        b"old"
    );
    assert!(!staging_download_path(&destination, nonce)
        .expect("staging path")
        .exists());
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn equal_length_altered_ota_is_published_without_a_catalog_hash_gate() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "5"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"wrong"))
        .mount(&server)
        .await;

    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let nonce = 17;
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), nonce);
    let downloaded = downloader
        .download_to_file(
            &server.uri(),
            &destination,
            1,
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("equal-length OTA content must not be rejected by a catalog hash");

    assert_eq!(downloaded, 5);
    assert_eq!(
        fs::read(&destination).expect("approved destination"),
        b"wrong"
    );
    assert!(!staging_download_path(&destination, nonce)
        .expect("staging path")
        .exists());
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn ota_commit_failure_removes_staging_and_preserves_nonempty_destination_directory() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "5"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fresh"))
        .mount(&server)
        .await;

    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::create_dir_all(&destination).expect("destination directory should be created");
    fs::write(destination.join("keep"), b"existing")
        .expect("destination directory should remain non-empty");
    let nonce = 19;
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), nonce);
    let error = downloader
        .download_to_file(
            &server.uri(),
            &destination,
            1,
            &CancellationToken::new(),
            None,
        )
        .await
        .expect_err("promoting over a non-empty directory must fail");

    assert!(error.to_string().contains("提交 OTA 下载结果失败"));
    assert_eq!(
        fs::read(destination.join("keep")).expect("destination directory must remain intact"),
        b"existing"
    );
    assert!(!staging_download_path(&destination, nonce)
        .expect("staging path")
        .exists());
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}

#[tokio::test]
async fn cancellation_after_staging_before_publish_preserves_destination() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", "5"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"fresh"))
        .mount(&server)
        .await;

    let directory = temporary_directory();
    fs::create_dir_all(&directory).expect("fixture directory should be created");
    let destination = directory.join("firmware.zip");
    fs::write(&destination, b"old").expect("old completed artifact should exist");
    let nonce = 18;
    let downloader = OtaDownloader::new(Client::new(), Arc::new(FixedDiskSpace(1_024)), nonce);
    let cancellation = CancellationToken::new();
    let cancellation_on_final_progress = cancellation.clone();
    let completed_reports = Arc::new(AtomicU64::new(0));
    let completed_reports_for_progress = Arc::clone(&completed_reports);
    let progress = move |report: OtaDownloadProgress| {
        if report.downloaded_bytes == report.total_bytes
            && completed_reports_for_progress.fetch_add(1, Ordering::Relaxed) == 1
        {
            cancellation_on_final_progress.cancel();
        }
    };

    let error = downloader
        .download_to_file(
            &server.uri(),
            &destination,
            1,
            &cancellation,
            Some(Arc::new(progress)),
        )
        .await
        .expect_err("cancellation after staging must prevent publication");

    assert_eq!(error, OtaDownloadError::Cancelled);
    assert_eq!(
        fs::read(&destination).expect("approved destination"),
        b"old"
    );
    assert!(!staging_download_path(&destination, nonce)
        .expect("staging path")
        .exists());
    fs::remove_dir_all(directory).expect("fixture directory should be removable");
}
