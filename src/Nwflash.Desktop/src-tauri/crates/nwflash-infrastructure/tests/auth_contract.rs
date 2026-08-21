use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

use nwflash_infrastructure::{AuthService, DEFAULT_APP_VERSION};

fn create_service(base_url: &str) -> AuthService {
    AuthService::new(base_url, DEFAULT_APP_VERSION)
}

#[tokio::test]
async fn login_successful_returns_token_and_user_info() {
    let mock_server = MockServer::start().await;
    let service = create_service(&mock_server.uri());
    let response_body = serde_json::json!({
        "token": "abcd",
        "username": "demo",
        "name": "演示用户"
    });

    Mock::given(method("POST"))
        .and(path("/api/login"))
        .and(body_json(serde_json::json!({
            "username": "demo",
            "password": "DemoPass123"
        })))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(response_body))
        .mount(&mock_server)
        .await;

    let session = service
        .login("demo", "DemoPass123")
        .await
        .expect("login should succeed");

    assert_eq!(session.token, "abcd");
    assert_eq!(session.name, "演示用户");
}

#[tokio::test]
async fn login_maps_non_200_to_api_error() {
    let mock_server = MockServer::start().await;
    let service = create_service(&mock_server.uri());

    Mock::given(method("POST"))
        .and(path("/api/login"))
        .and(body_json(serde_json::json!({
            "username": "demo",
            "password": "wrong"
        })))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"error": "用户名或密码错误"})),
        )
        .mount(&mock_server)
        .await;

    let err = service
        .login("demo", "wrong")
        .await
        .expect_err("wrong credential should fail");
    assert_eq!(err.status_code(), Some(401));
}

#[tokio::test]
async fn validate_token_returns_name_when_logged_in() {
    let mock_server = MockServer::start().await;
    let service = create_service(&mock_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/me"))
        .and(header("Authorization", "Bearer valid"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"loggedIn": true, "name": "演示用户"})),
        )
        .mount(&mock_server)
        .await;

    let name = service
        .validate_token("valid")
        .await
        .expect("validate token should be okay")
        .expect("logged in user has name");
    assert_eq!(name, "演示用户");
}

#[tokio::test]
async fn validate_token_returns_none_when_token_invalid_or_inactive() {
    let mock_server = MockServer::start().await;
    let service = create_service(&mock_server.uri());

    Mock::given(method("GET"))
        .and(path("/api/me"))
        .and(header("Authorization", "Bearer invalid"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "API token 无效或已停用。"
        })))
        .mount(&mock_server)
        .await;

    let name = service
        .validate_token("invalid")
        .await
        .expect("invalid token should be handled");
    assert!(name.is_none());
}
