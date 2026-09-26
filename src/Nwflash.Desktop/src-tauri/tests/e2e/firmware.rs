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
fn corrupt_payload_fails_without_publishing_partial_images() {
    // 内建解析器取代了外部 payload_dumper；仍然要保证：提取失败绝不在用户
    // 输出目录里留下半截镜像。这里用一个截断的 CrAU 头触发解码失败。
    let root = temporary_directory("payload-failure");
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let payload = root.join("broken.bin");
    // 头声明有 manifest，但后面什么都没有。
    let mut broken = Vec::new();
    broken.extend_from_slice(b"CrAU");
    broken.extend_from_slice(&2u64.to_be_bytes());
    broken.extend_from_slice(&4096u64.to_be_bytes());
    broken.extend_from_slice(&0u32.to_be_bytes());
    fs::write(&payload, &broken).expect("broken payload should be written");
    let output = root.join("output");

    let error = FirmwareExtractService::extract_payload(
        std::path::Path::new(""),
        payload.to_string_lossy().as_ref(),
        &["boot".to_string()],
        &output,
        || false,
    )
    .expect_err("损坏的 payload 必须失败");

    assert!(
        matches!(error, FirmwareExtractApplicationError::Format(_)),
        "实际: {error:?}"
    );
    let published = fs::read_dir(&output)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(published, 0, "失败时不得发布任何镜像");
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
