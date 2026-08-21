use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_infrastructure::{FirmwareFormat, FirmwareFormatDetector};
use tokio_util::sync::CancellationToken;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

#[test]
fn local_detector_routes_directory_and_known_firmware_magic_prefixes() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-firmware-format-{nonce}"));
    let images = root.join("images");
    fs::create_dir_all(&images).expect("image directory should be created");
    let gzip = root.join("vivo.gz");
    let zstd = root.join("vivo.zst");
    let zip = root.join("ota.zip");
    let payload = root.join("payload.bin");
    fs::write(&gzip, [0x1f, 0x8b, 0x08, 0x00]).expect("gzip fixture should be written");
    fs::write(&zstd, [0x28, 0xb5, 0x2f, 0xfd]).expect("zstd fixture should be written");
    fs::write(&zip, b"PK\x03\x04").expect("zip fixture should be written");
    fs::write(&payload, b"CrAU\0\0\0\0").expect("payload fixture should be written");

    assert_eq!(
        FirmwareFormatDetector::detect_local(&images).expect("directory should be detected"),
        FirmwareFormat::ImageDirectory
    );
    assert_eq!(
        FirmwareFormatDetector::detect_local(&gzip).expect("gzip should be detected"),
        FirmwareFormat::VivoGzipTar
    );
    assert_eq!(
        FirmwareFormatDetector::detect_local(&zstd).expect("zstd should be detected"),
        FirmwareFormat::VivoGzipTar
    );
    assert_eq!(
        FirmwareFormatDetector::detect_local(&zip).expect("zip should be detected"),
        FirmwareFormat::Zip
    );
    assert_eq!(
        FirmwareFormatDetector::detect_local(&payload).expect("payload should be detected"),
        FirmwareFormat::Payload
    );

    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn remote_payload_detection_reads_only_the_four_byte_magic_range() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/payload.bin"))
        .and(header("range", "bytes=0-3"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 0-3/987654321")
                .insert_header("content-length", "4")
                .set_body_bytes(b"CrAU"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let format =
        FirmwareFormatDetector::detect_remote_payload(&format!("{}/payload.bin", server.uri()))
            .await
            .expect("the four-byte remote payload magic should be accepted");

    assert_eq!(format, FirmwareFormat::Payload);
}

#[tokio::test]
async fn remote_payload_detection_rejects_a_server_that_ignores_range() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/payload.bin"))
        .and(header("range", "bytes=0-3"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "8")
                .set_body_bytes(b"CrAUrest"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error =
        FirmwareFormatDetector::detect_remote_payload(&format!("{}/payload.bin", server.uri()))
            .await
            .expect_err("a range-ignoring server must not permit an unbounded payload response");

    assert!(error.to_string().contains("Range"));
}

#[tokio::test]
async fn remote_payload_detection_honors_cancellation_before_the_response_arrives() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/payload.bin"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 0-3/4")
                .insert_header("content-length", "4")
                .set_delay(std::time::Duration::from_secs(1))
                .set_body_bytes(b"CrAU"),
        )
        .mount(&server)
        .await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = FirmwareFormatDetector::detect_remote_payload_with_cancel(
        &format!("{}/payload.bin", server.uri()),
        &cancellation,
    )
    .await
    .expect_err("a canceled probe must not wait for the remote response");

    assert!(matches!(
        error,
        nwflash_infrastructure::FirmwareExtractionError::Canceled
    ));
}
