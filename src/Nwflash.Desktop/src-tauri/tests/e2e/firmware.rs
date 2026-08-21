use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::{FirmwareExtractApplicationError, FirmwareExtractService};
use nwflash_infrastructure::{FirmwareExtractionError, FirmwareFormatDetector};
use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be available")
        .as_nanos();
    std::env::temp_dir().join(format!("nwflash-firmware-e2e-{label}-{nonce}"))
}

#[test]
fn payload_dumper_process_failure_propagates_and_removes_private_staging() {
    let root = temporary_directory("payload-failure");
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let executable = root.join("payload_dumper.cmd");
    let staging_record = root.join("staging-path.txt");
    let output = root.join("output");
    let script = format!(
        "@echo off\r\nset output=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\n>\"%output%\\partial.img\" echo partial\r\n>\"{}\" echo %output%\r\nexit /b 9\r\n",
        staging_record.display()
    );
    fs::write(&executable, script).expect("rejecting payload tool should be written");

    let error = FirmwareExtractService::extract_payload(
        &executable,
        "source.payload",
        &["boot".to_string(), "init_boot".to_string()],
        &output,
        || false,
    )
    .expect_err("nonzero payload_dumper exit must propagate through the extraction service");

    assert!(
        matches!(error, FirmwareExtractApplicationError::Format(message) if message.contains("9"))
    );
    let private_staging = PathBuf::from(
        fs::read_to_string(&staging_record)
            .expect("payload tool should record its staging path")
            .trim(),
    );
    assert!(!private_staging.exists());
    assert_eq!(
        fs::read_dir(&output)
            .expect("user output directory should be readable")
            .count(),
        0,
        "failed payload extraction must not publish partial images"
    );
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn malformed_firmware_download_range_is_rejected_before_payload_processing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/payload.bin"))
        .and(header("Range", "bytes=0-3"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("Content-Range", "bytes 1-4/1024")
                .set_body_bytes(b"CrAU"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error =
        FirmwareFormatDetector::detect_remote_payload(&format!("{}/payload.bin", server.uri()))
            .await
            .expect_err("an invalid range response must not be accepted as firmware metadata");

    assert!(matches!(error, FirmwareExtractionError::Io(message) if message.contains("Range")));
}
