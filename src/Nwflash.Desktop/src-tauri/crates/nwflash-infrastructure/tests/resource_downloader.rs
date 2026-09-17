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
async fn ordinary_third_party_http_client_remains_independent_and_falls_back() {
    let first_candidate = MockServer::start().await;
    let fallback_candidate = MockServer::start().await;

    // 首个候选源延迟 2s 才回应，用 1s 的候选上限把它判死，再验证回退源能补上。
    //
    // 这两个数字**不能收紧**：`per_candidate_timeout` 同时是「首个候选的失败阈值」
    // 和「回退源必须完成的预算」。早先用 80ms 延迟 / 20ms 上限时，回环往返本身
    // 就可能超过 20ms（环境里若设了 HTTP_PROXY，reqwest 默认会多一跳代理），
    // 于是连健康的回退源也被判超时 → 随机假失败（实测带代理 3/5、不带 1/5 失败）。
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
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
        Some(Duration::from_secs(1)),
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
