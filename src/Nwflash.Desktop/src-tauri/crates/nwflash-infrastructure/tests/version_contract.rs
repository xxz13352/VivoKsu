use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

use nwflash_infrastructure::{VersionCheckResult, VersionClient, DEFAULT_APP_VERSION};

fn create_client(base_url: &str) -> VersionClient {
    VersionClient::new(base_url, DEFAULT_APP_VERSION)
}

#[tokio::test]
async fn check_async_queries_app_version_check_endpoint() {
    let mock_server = MockServer::start().await;
    let client = create_client(&mock_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/app/version"))
        .and(query_param("current", DEFAULT_APP_VERSION))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest": "1.2.0",
            "min": "1.0.0",
            "download_url": "https://dl/1.2.0.zip",
            "update_required": true,
            "force_update": true
        })))
        .mount(&mock_server)
        .await;

    let result = client.check().await;
    assert_eq!(result.latest, Some("1.2.0".to_string()));
    assert_eq!(result.min_version, Some("1.0.0".to_string()));
    assert_eq!(
        result.download_url,
        Some("https://dl/1.2.0.zip".to_string())
    );
    assert!(result.update_required);
    assert!(result.force_update);
}

#[tokio::test]
async fn check_async_tolerates_missing_update_flags_and_keeps_other_fields() {
    let mock_server = MockServer::start().await;
    let client = create_client(&mock_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/app/version"))
        .and(query_param("current", DEFAULT_APP_VERSION))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest": "1.3.0",
            "min": "1.0.0",
            "download_url": "https://dl/1.3.0.zip"
        })))
        .mount(&mock_server)
        .await;

    let result = client.check().await;
    assert_eq!(result.latest, Some("1.3.0".to_string()));
    assert_eq!(result.min_version, Some("1.0.0".to_string()));
    assert_eq!(
        result.download_url,
        Some("https://dl/1.3.0.zip".to_string())
    );
    assert!(!result.update_required);
    assert!(!result.force_update);
}

#[tokio::test]
async fn repeated_version_checks_share_one_session_request() {
    let mock_server = MockServer::start().await;
    let client = create_client(&mock_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/app/version"))
        .and(query_param("current", DEFAULT_APP_VERSION))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "latest": "1.4.0",
            "min": "1.0.0",
            "download_url": "https://dl/1.4.0.zip"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let (first, second) = tokio::join!(client.check(), client.check());

    assert_eq!(first, second);
    mock_server.verify().await;
}

#[tokio::test]
async fn check_async_network_failure_returns_allow_all() {
    let mock_server = MockServer::start().await;
    let unavailable_uri = mock_server.uri();
    drop(mock_server);
    tokio::task::yield_now().await;

    let client = create_client(&unavailable_uri);
    let result = client.check().await;
    assert_eq!(result, VersionCheckResult::ALLOW_ALL);
}

#[tokio::test]
async fn check_async_maps_update_required_response_to_update_gate() {
    let mock_server = MockServer::start().await;
    let client = create_client(&mock_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/app/version"))
        .and(query_param("current", DEFAULT_APP_VERSION))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .respond_with(ResponseTemplate::new(426).set_body_json(serde_json::json!({
            "error": "请更新 Nwflash 到最新版本后继续使用。",
            "code": "UPDATE_REQUIRED",
            "latest": "2.0.0",
            "min": "1.5.0",
            "download_url": "https://x/Nwflash-2.0.0.zip"
        })))
        .mount(&mock_server)
        .await;

    let result = client.check().await;
    assert!(result.update_required);
    assert!(result.force_update);
    assert_eq!(result.latest, Some("2.0.0".to_string()));
    assert_eq!(result.min_version, Some("1.5.0".to_string()));
    assert_eq!(
        result.download_url,
        Some("https://x/Nwflash-2.0.0.zip".to_string())
    );
}
