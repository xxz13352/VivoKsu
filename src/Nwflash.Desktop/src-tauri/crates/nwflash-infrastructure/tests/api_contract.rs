use reqwest::header::AUTHORIZATION;
use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

use nwflash_domain::UsageLogEntry;
use nwflash_infrastructure::{
    CloudflareClient, CloudflareError, IntegrityFailure, IntegrityReportPhase,
    IntegrityReportReason, IntegrityReportRequest, ProcessIdentity, SecretToken,
    DEFAULT_APP_VERSION, DEFAULT_BASE_URL,
};

fn create_client(base_url: &str) -> CloudflareClient {
    CloudflareClient::new_injected(base_url, DEFAULT_APP_VERSION)
}

#[test]
fn authorization_header_is_fallible_sensitive_and_debug_redacted() {
    let api = create_client("http://127.0.0.1:1");
    let headers = api
        .authenticated_headers_for_test("header-secret")
        .expect("valid token should construct a header");
    let authorization = headers.get(AUTHORIZATION).unwrap();

    assert!(authorization.is_sensitive());
    assert!(!format!("{headers:?}").contains("header-secret"));
    let request = reqwest::Client::new()
        .get("http://127.0.0.1:1/private")
        .headers(headers)
        .build()
        .unwrap();
    assert!(!format!("{request:?}").contains("header-secret"));
    assert!(api.authenticated_headers_for_test("bad\nheader").is_err());
}

fn process_identity() -> ProcessIdentity {
    ProcessIdentity::new_injected("build-contract", "nonce-contract").unwrap()
}

fn integrity_report_request() -> IntegrityReportRequest {
    IntegrityReportRequest {
        event_id: "integrity-1787444800000-1".to_string(),
        phase: IntegrityReportPhase::Heartbeat,
        reason: IntegrityReportReason::LeaseSignatureInvalid,
        client_version: DEFAULT_APP_VERSION.to_string(),
        build_id: "build-contract".to_string(),
        occurred_at: 1_787_444_800,
    }
}

#[tokio::test]
async fn integrity_report_posts_exact_six_field_anonymous_body_once() {
    let server = MockServer::start().await;
    let api = create_client(&server.uri());
    Mock::given(method("POST"))
        .and(path("/api/integrity/report"))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .and(body_json(serde_json::json!({
            "event_id": "integrity-1787444800000-1",
            "phase": "heartbeat",
            "reason": "lease_signature_invalid",
            "client_version": DEFAULT_APP_VERSION,
            "build_id": "build-contract",
            "occurred_at": 1787444800
        })))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;

    api.report_integrity(None, &integrity_report_request())
        .await
        .expect("202 should accept an anonymous report");

    let requests = server
        .received_requests()
        .await
        .expect("request capture should succeed");
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key(AUTHORIZATION));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 6);
}

#[tokio::test]
async fn integrity_report_uses_one_sensitive_bearer_without_serializing_it() {
    let server = MockServer::start().await;
    let api = create_client(&server.uri());
    let token = SecretToken::new("report-bearer-secret".to_string());
    Mock::given(method("POST"))
        .and(path("/api/integrity/report"))
        .and(header("Authorization", "Bearer report-bearer-secret"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    api.report_integrity(Some(&token), &integrity_report_request())
        .await
        .expect("every 2xx response should be accepted");

    let body = serde_json::to_string(&integrity_report_request()).unwrap();
    assert!(!body.contains("report-bearer-secret"));
    for prohibited in [
        "token", "password", "path", "url", "serial", "output", "detail",
    ] {
        assert!(!body.contains(prohibited));
    }
}

#[tokio::test]
async fn invalid_integrity_report_is_rejected_before_any_http_request() {
    let server = MockServer::start().await;
    let api = create_client(&server.uri());
    let mut invalid = integrity_report_request();
    invalid.event_id = "contains spaces".to_string();

    let error = api
        .report_integrity(None, &invalid)
        .await
        .expect_err("invalid event identifiers must fail locally");

    assert!(matches!(error, CloudflareError::InvalidInput(_)));
    assert!(server
        .received_requests()
        .await
        .expect("request capture should succeed")
        .is_empty());
}

#[tokio::test]
async fn integrity_report_http_failure_is_redacted_and_not_retried() {
    let server = MockServer::start().await;
    let api = create_client(&server.uri());
    Mock::given(method("POST"))
        .and(path("/api/integrity/report"))
        .respond_with(ResponseTemplate::new(401).set_body_string("server-response-secret"))
        .expect(1)
        .mount(&server)
        .await;

    let error = api
        .report_integrity(None, &integrity_report_request())
        .await
        .expect_err("401 should be a terminal report outcome");

    assert_eq!(error.status_code(), Some(401));
    assert!(!format!("{error:?} {error}").contains("server-response-secret"));
}

#[test]
fn production_default_is_pinned_and_fails_closed_without_a_compile_time_verification_key() {
    let result: Result<CloudflareClient, CloudflareError> = CloudflareClient::new_default();
    match result {
        Ok(client) => {
            assert!(option_env!("NWFLASH_SESSION_VERIFY_KEY_B64").is_some());
            assert_eq!(client.base_url(), DEFAULT_BASE_URL);
        }
        Err(CloudflareError::Integrity(IntegrityFailure::MissingVerificationKey)) => {
            assert!(option_env!("NWFLASH_SESSION_VERIFY_KEY_B64").is_none());
        }
        Err(CloudflareError::Integrity(IntegrityFailure::InvalidVerificationKey)) => {
            assert!(option_env!("NWFLASH_SESSION_VERIFY_KEY_B64").is_some());
        }
        other => panic!("unexpected protected production construction result: {other:?}"),
    }
}

#[tokio::test]
async fn explicit_injected_client_remains_available_for_local_http_adapters() {
    let mock_server = MockServer::start().await;
    let api = create_client(&mock_server.uri());
    Mock::given(method("GET"))
        .and(path("/api/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "update_required": false,
            "force_update": false
        })))
        .mount(&mock_server)
        .await;

    api.check_version_policy()
        .await
        .expect("explicit injected client should support local HTTP test adapters");
}

#[tokio::test]
async fn login_response_carries_both_signed_lease_strings_without_admission() {
    let mock_server = MockServer::start().await;
    let api = create_client(&mock_server.uri());
    Mock::given(method("POST"))
        .and(path("/api/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": "tok",
            "username": "demo",
            "name": "Demo",
            "lease_payload": "payload-ascii",
            "lease_signature": "signature"
        })))
        .mount(&mock_server)
        .await;

    let result = api
        .login("demo", "password", &process_identity(), "session")
        .await
        .expect("login response should deserialize");
    assert_eq!(result.lease_payload, "payload-ascii");
    assert_eq!(result.lease_signature, "signature");
}

#[tokio::test]
async fn heartbeat_response_carries_both_signed_lease_strings_without_admission() {
    let mock_server = MockServer::start().await;
    let api = create_client(&mock_server.uri());
    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "force_exit": false,
            "lease_payload": "heartbeat-payload",
            "lease_signature": "heartbeat-signature"
        })))
        .mount(&mock_server)
        .await;

    let result = api
        .heartbeat("tok", &process_identity(), "session", 1, true)
        .await
        .expect("heartbeat response should deserialize");
    assert_eq!(result.lease_payload, "heartbeat-payload");
    assert_eq!(result.lease_signature, "heartbeat-signature");
}

#[tokio::test]
async fn resolve_async_queries_the_server_with_pd_and_version_and_deserializes_the_rom() {
    let mock_server = MockServer::start().await;

    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    let rom_body = serde_json::json!({
        "pd": "PD2057",
        "version": "16.2.10.0.W10.V000L1",
        "url": "https://sysuptxdl.vivo.com.cn/full.zip",
        "name": "full",
        "sizeBytes": 1024
    });

    Mock::given(method("GET"))
        .and(path("/api/rom"))
        .and(query_param("pd", "PD2057"))
        .and(query_param("version", "16.2.10.0/W10.V000L1"))
        .and(header("Authorization", "Bearer tok"))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .and(path_regex(r"^/api/rom$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(rom_body))
        .mount(&mock_server)
        .await;

    let rom = api
        .resolve_rom("tok", "PD2057", "16.2.10.0/W10.V000L1")
        .await
        .expect("resolve_rom should deserialize response");

    assert_eq!(rom.pd, "PD2057");
    assert_eq!(rom.size_bytes, Some(1024));
    assert_eq!(rom.version, "16.2.10.0.W10.V000L1");
}

#[tokio::test]
async fn resolve_async_escapes_special_characters_in_query_parameters() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("GET"))
        .and(path("/api/rom"))
        .and(header("Authorization", "Bearer tok"))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "pd": "PD 2057",
            "version": "16.2.10.0/W30",
            "url": "https://x/y"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let _ = api
        .resolve_rom("tok", "PD 2057", "16.2.10.0/W30")
        .await
        .expect("should pass encoded query");

    let received = mock_server
        .received_requests()
        .await
        .expect("received request");
    assert_eq!(received.len(), 1);
    let request = &received[0];
    let path = request.url.path();
    let query = request.url.query().unwrap_or("");
    let _path_and_query = if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    };
    assert!(query.contains("pd=PD%202057"));
    assert!(query.contains("version=16.2.10.0%2FW30"));
}

#[tokio::test]
async fn resolve_async_maps_not_found_to_chinese_message() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("GET"))
        .and(path("/api/rom"))
        .and(header("Authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "未找到 PD2057 对应的 ROM。"
        })))
        .mount(&mock_server)
        .await;

    let error = api
        .resolve_rom("tok", "PD2057", "nope")
        .await
        .expect_err("not found should be mapped");

    assert_eq!(error.status_code(), Some(404));
    assert!(format!("{error}").contains("未找到"));
}

#[tokio::test]
async fn resolve_async_maps_insufficient_credits_status() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("GET"))
        .and(path("/api/rom"))
        .and(header("Authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(402).set_body_json(serde_json::json!({
            "error": "VOTA 未能解析 ROM 下载链接。(INSUFFICIENT_CREDITS)"
        })))
        .mount(&mock_server)
        .await;

    let error = api
        .resolve_rom("tok", "PD2057", "v")
        .await
        .expect_err("insufficient credits should be mapped");

    assert_eq!(error.status_code(), Some(402));
    assert!(format!("{error}").contains("信用点不足"));
    assert!(!format!("{error}").contains("INSUFFICIENT_CREDITS"));
}

#[tokio::test]
async fn active_heartbeat_posts_the_complete_signed_session_binding() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);
    let token = "tok";

    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .and(header("Authorization", format!("Bearer {token}")))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .and(body_json(serde_json::json!({
            "session_id": "sess-abc",
            "client_version": DEFAULT_APP_VERSION,
            "build_id": "build-contract",
            "process_nonce": "nonce-contract",
            "sequence": 1,
            "active": true
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"force_exit":false})),
        )
        .mount(&mock_server)
        .await;

    let heartbeat = api
        .heartbeat(token, &process_identity(), "sess-abc", 1, true)
        .await
        .expect("heartbeat should parse response");
    assert!(!heartbeat.force_exit);
}

#[tokio::test]
async fn heartbeat_parses_force_exit_reason() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"force_exit": true, "reason": "违规下线"})),
        )
        .mount(&mock_server)
        .await;

    let heartbeat = api
        .heartbeat("tok", &process_identity(), "sess-abc", 1, true)
        .await
        .expect("heartbeat should parse forced exit");
    assert!(heartbeat.force_exit);
    assert_eq!(heartbeat.reason.as_deref(), Some("违规下线"));
}

#[tokio::test]
async fn goodbye_posts_only_the_authenticated_session_id_and_inactive_flag() {
    let server = MockServer::start().await;
    let api = create_client(&server.uri());
    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .and(header("Authorization", "Bearer tok"))
        .and(body_json(serde_json::json!({
            "session_id": "sess-abc",
            "active": false
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"force_exit": false})),
        )
        .mount(&server)
        .await;

    api.heartbeat("tok", &process_identity(), "sess-abc", 9, false)
        .await
        .expect("authenticated goodbye should parse");
}

#[tokio::test]
async fn heartbeat_maps_426_to_update_required() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .respond_with(ResponseTemplate::new(426).set_body_json(serde_json::json!({
            "error":"请更新 VivoKsu 到最新版本后继续使用。",
            "code":"UPDATE_REQUIRED",
            "latest":"2.0.0",
            "min":"1.0.0",
            "download_url":"https://x/VivoKsu-2.0.0.zip"
        })))
        .mount(&mock_server)
        .await;

    let err = api
        .heartbeat("tok", &process_identity(), "sess-abc", 1, true)
        .await
        .expect_err("426 should map to update required");
    match err {
        CloudflareError::UpdateRequired(info) => {
            assert_eq!(info.latest, Some("2.0.0".to_string()));
            assert_eq!(info.min_version, Some("1.0.0".to_string()));
            assert_eq!(
                info.download_url,
                Some("https://x/VivoKsu-2.0.0.zip".to_string())
            );
        }
        other => panic!("expect update required, got {other}"),
    }
}

#[tokio::test]
async fn get_online_deserializes_sessions_and_self_flag() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("GET"))
        .and(path("/api/online"))
        .and(header("Authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 2,
            "sessions": [
                {
                    "name":"张三",
                    "client_version":"1.0.0",
                    "connected_at":1000,
                    "last_seen_at":1000,
                    "duration_seconds":3600,
                    "is_self":true
                },
                {
                    "name":"李四",
                    "client_version":"1.0.0",
                    "connected_at":2000,
                    "last_seen_at":2000,
                    "duration_seconds":1800,
                    "is_self":false
                }
            ]
        })))
        .mount(&mock_server)
        .await;

    let sessions = api
        .get_online("tok")
        .await
        .expect("online sessions should parse");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].name, "张三");
    assert!(sessions[0].is_self);
    assert_eq!(sessions[1].name, "李四");
    assert!(!sessions[1].is_self);
}

#[tokio::test]
async fn get_online_parses_lenient_string_numbers_and_non_bool_self_flag() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("GET"))
        .and(path("/api/online"))
        .and(header("Authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sessions": [
                {
                    "name":"王五",
                    "client_version":"1.0.0",
                    "connected_at":"1724000000",
                    "last_seen_at":"1723999999",
                    "duration_seconds":"1800",
                    "is_self":"true"
                },
                {
                    "name":"赵六",
                    "client_version":"1.0.0",
                    "connected_at": 99,
                    "is_self": false
                }
            ]
        })))
        .mount(&mock_server)
        .await;

    let sessions = api
        .get_online("tok")
        .await
        .expect("string-typed numbers and non-bool self flag should parse leniently");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].connected_at, 1_724_000_000);
    assert_eq!(sessions[0].last_seen_at, 1_723_999_999);
    assert_eq!(sessions[0].duration_seconds, 1800);
    // Only a literal JSON `true` counts as self; a string "true" is false.
    assert!(!sessions[0].is_self);
    // The second session omits last_seen_at/duration_seconds -> default 0.
    assert_eq!(sessions[1].connected_at, 99);
    assert_eq!(sessions[1].last_seen_at, 0);
    assert_eq!(sessions[1].duration_seconds, 0);
    assert!(!sessions[1].is_self);
}

#[tokio::test]
async fn authorize_operation_posts_operation_and_parses_allowed() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("POST"))
        .and(path("/api/operation/authorize"))
        .and(header("Authorization", "Bearer tok"))
        .and(body_json(serde_json::json!({
            "operation": "Flashing",
            "title": "正在刷写 boot"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "allowed": true
        })))
        .mount(&mock_server)
        .await;

    let result = api
        .authorize_operation("tok", "Flashing", "正在刷写 boot")
        .await
        .expect("authorize should parse");
    assert!(result.allowed);
}

#[tokio::test]
async fn authorize_operation_maps_401_to_api_error() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("POST"))
        .and(path("/api/operation/authorize"))
        .and(header("Authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "API token 无效或已停用。"
        })))
        .mount(&mock_server)
        .await;

    let err = api
        .authorize_operation("tok", "Flashing", "正在刷写 boot")
        .await
        .expect_err("401 should map");
    assert_eq!(err.status_code(), Some(401));
}

#[tokio::test]
async fn upload_usage_logs_posts_the_batch() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);
    let logs = vec![
        UsageLogEntry {
            operation: "Flashing".to_string(),
            title: "正在刷写 boot".to_string(),
            status: "success".to_string(),
            event_id: "evt-1".to_string(),
            started_at: 1000,
            ended_at: Some(1060),
            duration_ms: Some(60000),
        },
        UsageLogEntry {
            operation: "Rebooting".to_string(),
            title: "正在重启设备".to_string(),
            status: "failed".to_string(),
            event_id: "evt-2".to_string(),
            started_at: 2000,
            ended_at: Some(2010),
            duration_ms: Some(10000),
        },
    ];

    Mock::given(method("POST"))
        .and(path("/api/usage/logs"))
        .and(header("Authorization", "Bearer tok"))
        .and(body_json(serde_json::json!({
            "logs": [
                {
                    "operation": "Flashing",
                    "title": "正在刷写 boot",
                    "status": "success",
                    "event_id": "evt-1",
                    "started_at": 1000,
                    "ended_at": 1060,
                    "duration_ms": 60000
                },
                {
                    "operation": "Rebooting",
                    "title": "正在重启设备",
                    "status": "failed",
                    "event_id": "evt-2",
                    "started_at": 2000,
                    "ended_at": 2010,
                    "duration_ms": 10000
                }
            ]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "received": 2
        })))
        .mount(&mock_server)
        .await;

    let response = api
        .upload_usage_logs("tok", &logs)
        .await
        .expect("upload usage logs should parse");
    assert!(response.ok);
}

#[tokio::test]
async fn heartbeat_maps_403_to_api_error_before_json_parse() {
    let mock_server = MockServer::start().await;
    let base_url = mock_server.uri();
    let api = create_client(&base_url);

    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .respond_with(
            ResponseTemplate::new(403).set_body_string("<html><body>Forbidden</body></html>"),
        )
        .mount(&mock_server)
        .await;

    let err = api
        .heartbeat("tok", &process_identity(), "sess-abc", 1, true)
        .await
        .expect_err("forbidden html should map to api error");
    assert_eq!(err.status_code(), Some(403));
}
