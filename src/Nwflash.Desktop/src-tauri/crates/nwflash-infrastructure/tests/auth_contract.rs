use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use ed25519_dalek::{Signer as _, SigningKey};
use nwflash_infrastructure::{
    AuthService, CloudflareClient, HeartbeatAdmission, IntegrityFailure, LoginRequest,
    ProcessIdentity, SecretToken, DEFAULT_APP_VERSION,
};
use nwflash_protection::{LeaseClaims, LeaseKind, TokenDigest};
use rand_core::OsRng;
use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};

const USERNAME: &str = "demo";
const PASSWORD: &str = "DemoPass123";
const TOKEN: &str = "runtime-token";
const BUILD_ID: &str = "build-runtime";
const PROCESS_NONCE: &str = "nonce-runtime";
const SESSION_ID: &str = "session-runtime";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_secs() as i64
}

fn identity() -> ProcessIdentity {
    ProcessIdentity::new_injected(BUILD_ID, PROCESS_NONCE).expect("test identity should be valid")
}

#[allow(clippy::too_many_arguments)]
fn claims(
    kind: LeaseKind,
    token: &str,
    build_id: &str,
    process_nonce: &str,
    session_id: &str,
    sequence: u64,
    issued_at: i64,
    expires_at: i64,
) -> LeaseClaims {
    LeaseClaims {
        version: 1,
        kind,
        username: USERNAME.to_string(),
        token_sha256: TokenDigest::sha256(token.as_bytes()),
        client_version: DEFAULT_APP_VERSION.to_string(),
        build_id: build_id.to_string(),
        process_nonce: process_nonce.to_string(),
        session_id: session_id.to_string(),
        sequence,
        issued_at,
        expires_at,
    }
}

fn sign(signing_key: &SigningKey, claims: LeaseClaims) -> (String, String) {
    let payload = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&claims).expect("runtime-generated claims should serialize"));
    let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
    (payload, signature)
}

fn create_service(base_url: &str, signing_key: &SigningKey) -> AuthService {
    AuthService::with_client(CloudflareClient::new_injected_with_lease_key(
        base_url,
        DEFAULT_APP_VERSION,
        signing_key.verifying_key(),
    ))
}

async fn mount_login(
    server: &MockServer,
    lease_payload: String,
    lease_signature: String,
    response_token: &str,
) {
    Mock::given(method("POST"))
        .and(path("/api/login"))
        .and(body_json(serde_json::json!({
            "username": USERNAME,
            "password": PASSWORD,
            "client_version": DEFAULT_APP_VERSION,
            "build_id": BUILD_ID,
            "process_nonce": PROCESS_NONCE,
            "session_id": SESSION_ID
        })))
        .and(header("X-Nwflash-Version", DEFAULT_APP_VERSION))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "token": response_token,
            "username": USERNAME,
            "name": "演示用户",
            "lease_payload": lease_payload,
            "lease_signature": lease_signature
        })))
        .mount(server)
        .await;
}

async fn login_with_lease(
    service: &AuthService,
    identity: &ProcessIdentity,
) -> nwflash_infrastructure::AuthSession {
    service
        .login(USERNAME, PASSWORD, identity, SESSION_ID)
        .await
        .expect("signed login should be admitted")
}

#[tokio::test]
async fn signed_login_posts_the_complete_binding_and_admits_sequence_one() {
    let server = MockServer::start().await;
    let signing_key = SigningKey::generate(&mut OsRng);
    let issued_at = now();
    let (payload, signature) = sign(
        &signing_key,
        claims(
            LeaseKind::Login,
            TOKEN,
            BUILD_ID,
            PROCESS_NONCE,
            SESSION_ID,
            1,
            issued_at,
            issued_at + 300,
        ),
    );
    mount_login(&server, payload, signature, TOKEN).await;

    let session = login_with_lease(&create_service(&server.uri(), &signing_key), &identity()).await;

    assert_eq!(session.token.as_str(), TOKEN);
    assert_eq!(session.name, "演示用户");
    assert_eq!(session.lease.session_id(), SESSION_ID);
    assert_eq!(session.lease.sequence(), 1);
}

#[tokio::test]
async fn unsigned_and_tampered_login_envelopes_fail_closed() {
    for tampered in [false, true] {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        let issued_at = now();
        let (mut payload, signature) = if tampered {
            sign(
                &signing_key,
                claims(
                    LeaseKind::Login,
                    TOKEN,
                    BUILD_ID,
                    PROCESS_NONCE,
                    SESSION_ID,
                    1,
                    issued_at,
                    issued_at + 300,
                ),
            )
        } else {
            (String::new(), String::new())
        };
        if tampered {
            payload.push('A');
        }
        mount_login(&server, payload, signature, TOKEN).await;

        let error = create_service(&server.uri(), &signing_key)
            .login(USERNAME, PASSWORD, &identity(), SESSION_ID)
            .await
            .expect_err("unverified login must fail closed");

        assert!(matches!(
            error,
            nwflash_infrastructure::CloudflareError::Integrity(
                IntegrityFailure::LeaseEnvelope | IntegrityFailure::LeaseSignature
            )
        ));
    }
}

#[tokio::test]
async fn mismatched_or_expired_login_lease_fails_closed() {
    let cases = [
        (
            "wrong build",
            "other-build",
            PROCESS_NONCE,
            SESSION_ID,
            TOKEN,
            300,
        ),
        (
            "wrong nonce",
            BUILD_ID,
            "other-nonce",
            SESSION_ID,
            TOKEN,
            300,
        ),
        (
            "wrong session",
            BUILD_ID,
            PROCESS_NONCE,
            "other-session",
            TOKEN,
            300,
        ),
        (
            "wrong token",
            BUILD_ID,
            PROCESS_NONCE,
            SESSION_ID,
            "other-token",
            300,
        ),
        ("expired", BUILD_ID, PROCESS_NONCE, SESSION_ID, TOKEN, -1),
    ];

    for (name, build_id, nonce, session_id, signed_token, ttl) in cases {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        let issued_at = now() - i64::from(ttl < 0);
        let (payload, signature) = sign(
            &signing_key,
            claims(
                LeaseKind::Login,
                signed_token,
                build_id,
                nonce,
                session_id,
                1,
                issued_at,
                issued_at + ttl,
            ),
        );
        mount_login(&server, payload, signature, TOKEN).await;

        let error = create_service(&server.uri(), &signing_key)
            .login(USERNAME, PASSWORD, &identity(), SESSION_ID)
            .await
            .expect_err(name);

        assert!(matches!(
            error,
            nwflash_infrastructure::CloudflareError::Integrity(
                IntegrityFailure::LeaseBinding | IntegrityFailure::LeaseTime
            )
        ));
    }
}

#[tokio::test]
async fn signed_login_sequence_other_than_one_fails_closed() {
    let server = MockServer::start().await;
    let signing_key = SigningKey::generate(&mut OsRng);
    let issued_at = now();
    let (payload, signature) = sign(
        &signing_key,
        claims(
            LeaseKind::Login,
            TOKEN,
            BUILD_ID,
            PROCESS_NONCE,
            SESSION_ID,
            2,
            issued_at,
            issued_at + 300,
        ),
    );
    mount_login(&server, payload, signature, TOKEN).await;

    let error = create_service(&server.uri(), &signing_key)
        .login(USERNAME, PASSWORD, &identity(), SESSION_ID)
        .await
        .expect_err("login sequence must begin at one");

    assert!(matches!(
        error,
        nwflash_infrastructure::CloudflareError::Integrity(IntegrityFailure::LeaseSequence)
    ));
}

async fn admitted_login(
    server: &MockServer,
    signing_key: &SigningKey,
) -> (
    AuthService,
    ProcessIdentity,
    nwflash_infrastructure::AuthSession,
) {
    let issued_at = now();
    let (payload, signature) = sign(
        signing_key,
        claims(
            LeaseKind::Login,
            TOKEN,
            BUILD_ID,
            PROCESS_NONCE,
            SESSION_ID,
            1,
            issued_at,
            issued_at + 300,
        ),
    );
    mount_login(server, payload, signature, TOKEN).await;
    let service = create_service(&server.uri(), signing_key);
    let identity = identity();
    let session = login_with_lease(&service, &identity).await;
    (service, identity, session)
}

#[tokio::test]
async fn signed_heartbeat_sends_the_current_sequence_and_advances_the_lease() {
    let server = MockServer::start().await;
    let signing_key = SigningKey::generate(&mut OsRng);
    let (service, identity, session) = admitted_login(&server, &signing_key).await;
    let issued_at = now();
    let (payload, signature) = sign(
        &signing_key,
        claims(
            LeaseKind::Heartbeat,
            TOKEN,
            BUILD_ID,
            PROCESS_NONCE,
            SESSION_ID,
            2,
            issued_at,
            issued_at + 300,
        ),
    );
    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .and(header("Authorization", format!("Bearer {TOKEN}")))
        .and(body_json(serde_json::json!({
            "session_id": SESSION_ID,
            "client_version": DEFAULT_APP_VERSION,
            "build_id": BUILD_ID,
            "process_nonce": PROCESS_NONCE,
            "sequence": 1,
            "active": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "force_exit": false,
            "lease_payload": payload,
            "lease_signature": signature
        })))
        .mount(&server)
        .await;

    let heartbeat = service
        .heartbeat(
            &session.token,
            &session.username,
            &identity,
            &session.lease,
            true,
        )
        .await
        .expect("signed heartbeat should be admitted");

    match heartbeat {
        HeartbeatAdmission::Accepted(lease) => assert_eq!(lease.sequence(), 2),
        other => panic!("expected accepted heartbeat, got {other:?}"),
    }
}

#[tokio::test]
async fn signed_heartbeat_sequence_rollback_is_terminal_integrity_failure() {
    let server = MockServer::start().await;
    let signing_key = SigningKey::generate(&mut OsRng);
    let (service, identity, session) = admitted_login(&server, &signing_key).await;
    let issued_at = now();
    let (payload, signature) = sign(
        &signing_key,
        claims(
            LeaseKind::Heartbeat,
            TOKEN,
            BUILD_ID,
            PROCESS_NONCE,
            SESSION_ID,
            1,
            issued_at,
            issued_at + 300,
        ),
    );
    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "force_exit": false,
            "lease_payload": payload,
            "lease_signature": signature
        })))
        .mount(&server)
        .await;

    let error = service
        .heartbeat(
            &session.token,
            &session.username,
            &identity,
            &session.lease,
            true,
        )
        .await
        .expect_err("rollback must fail closed");
    assert!(matches!(
        error,
        nwflash_infrastructure::CloudflareError::Integrity(IntegrityFailure::LeaseSequence)
    ));
}

#[tokio::test]
async fn force_exit_heartbeat_is_terminal_without_a_signed_envelope() {
    let server = MockServer::start().await;
    let signing_key = SigningKey::generate(&mut OsRng);
    let (service, identity, session) = admitted_login(&server, &signing_key).await;
    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "force_exit": true,
            "reason": "terminal fixture"
        })))
        .mount(&server)
        .await;

    let admission = service
        .heartbeat(
            &session.token,
            &session.username,
            &identity,
            &session.lease,
            true,
        )
        .await
        .expect("force exit is a terminal server admission");

    assert_eq!(
        admission,
        HeartbeatAdmission::ForceExit("terminal fixture".to_string())
    );
}

#[tokio::test]
async fn unsigned_active_heartbeat_fails_closed() {
    let server = MockServer::start().await;
    let signing_key = SigningKey::generate(&mut OsRng);
    let (service, identity, session) = admitted_login(&server, &signing_key).await;
    Mock::given(method("POST"))
        .and(path("/api/heartbeat"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "force_exit": false
        })))
        .mount(&server)
        .await;

    let error = service
        .heartbeat(
            &session.token,
            &session.username,
            &identity,
            &session.lease,
            true,
        )
        .await
        .expect_err("unsigned active heartbeat must fail closed");

    assert!(matches!(
        error,
        nwflash_infrastructure::CloudflareError::Integrity(IntegrityFailure::LeaseEnvelope)
    ));
}

#[test]
fn secret_debug_is_redacted_and_explicit_zeroization_clears_storage() {
    let mut token = SecretToken::new("never-print-me".to_string());
    assert!(!format!("{token:?}").contains("never-print-me"));
    token.zeroize();
    assert!(token.is_empty());

    let request = LoginRequest::new(
        USERNAME,
        PASSWORD,
        DEFAULT_APP_VERSION,
        BUILD_ID,
        PROCESS_NONCE,
        SESSION_ID,
    );
    assert!(!format!("{request:?}").contains(PASSWORD));
}

#[tokio::test]
async fn login_maps_non_200_to_api_error() {
    let server = MockServer::start().await;
    let signing_key = SigningKey::generate(&mut OsRng);
    let service = create_service(&server.uri(), &signing_key);
    Mock::given(method("POST"))
        .and(path("/api/login"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({"error": "用户名或密码错误"})),
        )
        .mount(&server)
        .await;

    let error = service
        .login(USERNAME, "wrong", &identity(), SESSION_ID)
        .await
        .expect_err("wrong credential should fail");
    assert_eq!(error.status_code(), Some(401));
}
