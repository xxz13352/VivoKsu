use serde::Serialize;
use tauri::State;
use zeroize::Zeroizing;

use crate::AppState;
use nwflash_application::{
    SessionLifecycleError, SessionLifecycleSession, OPERATION_IN_PROGRESS_MESSAGE,
};
use nwflash_infrastructure::{AuthSession, SecretToken};

#[derive(Serialize)]
pub struct AuthSessionDto {
    pub username: String,
    pub name: String,
    pub generation: String,
}

#[derive(Serialize)]
pub struct LogoutResult {
    pub ok: bool,
}

#[tauri::command]
pub async fn auth_login(
    state: State<'_, AppState>,
    username: String,
    password: String,
) -> Result<AuthSessionDto, String> {
    // Move the WebView-owned password into zeroizing Rust storage before any await/error path.
    let password = Zeroizing::new(password);
    auth_login_inner(&state, username, password).await
}

async fn auth_login_inner(
    state: &AppState,
    username: String,
    password: Zeroizing<String>,
) -> Result<AuthSessionDto, String> {
    let session_id = state
        .process_identity
        .fresh_session_id()
        .map_err(|error| error.to_string())?;
    let generation = state
        .process_identity
        .fresh_generation()
        .map_err(|error| error.to_string())?;
    let session = match state
        .auth_service
        .login(
            &username,
            password.as_str(),
            &state.process_identity,
            &session_id,
        )
        .await
    {
        Ok(session) => session,
        Err(error @ nwflash_infrastructure::CloudflareError::Integrity(_)) => {
            let nwflash_infrastructure::CloudflareError::Integrity(failure) = &error else {
                unreachable!();
            };
            let request = crate::exit_request_for_integrity_at(
                None,
                failure.clone(),
                crate::exit_supervisor::ExitPhase::Login,
            );
            let _ = state.exit_supervisor.request(request);
            return Err(error.user_message());
        }
        Err(error) => return Err(error.user_message()),
    };
    state
        .protection
        .verify_safe_point(crate::exit_supervisor::ExitPhase::Login)
        .map_err(|_| "本地完整性校验失败，无法建立会话。".to_string())?;
    let dto = AuthSessionDto {
        username: session.username.clone(),
        name: session.name.clone(),
        generation: generation.clone(),
    };

    finalize_login_session(state, session, generation).await?;
    Ok(dto)
}

async fn finalize_login_session(
    state: &AppState,
    session: AuthSession,
    generation: String,
) -> Result<(), String> {
    let lifecycle_session = SessionLifecycleSession::prepare(
        session.token.request_scope(),
        session.username.clone(),
        session.lease.clone(),
        generation.clone(),
    )
    .map_err(|error| error.to_string())?;
    let idle_lease = state
        .operation_coordinator
        .try_acquire_idle()
        .map_err(|_| OPERATION_IN_PROGRESS_MESSAGE.to_string())?;

    match state.session_lifecycle.stop().await {
        Ok(()) | Err(SessionLifecycleError::NotStarted) => {}
        Err(error) => return Err(error.to_string()),
    }
    state.usage_reporter.flush().await;
    state.revoke_root_capabilities(&idle_lease);
    state
        .exit_supervisor
        .install_generation(generation.clone())
        .map_err(|_| "应用正在安全退出，无法发布新会话。".to_string())?;

    let capability =
        state
            .session_capabilities
            .activate_verified(generation, session.username, session.lease);
    {
        let mut token = state
            .session_token
            .write()
            .expect("session token lock should not be poisoned");
        let _ = replace_session_token(&mut token, session.token);
    }

    state
        .session_lifecycle
        .start_prepared(lifecycle_session)
        .await;
    debug_assert!(state.session_capabilities.is_current(capability));
    drop(idle_lease);
    state.usage_reporter.start_if_needed();
    Ok(())
}

#[tauri::command]
pub async fn auth_logout(state: State<'_, AppState>) -> Result<LogoutResult, String> {
    auth_logout_inner(&state).await
}

async fn auth_logout_inner(state: &AppState) -> Result<LogoutResult, String> {
    let generation = state.session_lifecycle.generation().await;
    let idle_lease = state
        .operation_coordinator
        .try_acquire_idle()
        .map_err(|_| OPERATION_IN_PROGRESS_MESSAGE.to_string())?;

    match state.session_lifecycle.stop().await {
        Ok(()) | Err(SessionLifecycleError::NotStarted) => {}
        Err(error) => return Err(error.to_string()),
    }
    state.usage_reporter.flush().await;
    state.revoke_root_capabilities(&idle_lease);
    {
        let mut token = state
            .session_token
            .write()
            .expect("session token lock should not be poisoned");
        let _ = clear_session_token(&mut token);
    }
    if let Some(generation) = generation.as_deref() {
        state.exit_supervisor.clear_generation(generation);
    }
    Ok(LogoutResult { ok: true })
}

#[tauri::command]
pub async fn auth_validate_token(state: State<'_, AppState>) -> Result<Option<String>, String> {
    state
        .session_capabilities
        .security()
        .map_err(|_| "未登录，无法校验会话。".to_string())?;
    let token = state
        .session_token
        .read()
        .expect("session token lock should not be poisoned")
        .as_ref()
        .map(SecretToken::request_scope);

    match token {
        Some(token) => state
            .auth_service
            .validate_token(token.as_str())
            .await
            .map_err(|error| error.user_message()),
        None => Ok(None),
    }
}

fn replace_session_token(
    slot: &mut Option<SecretToken>,
    replacement: SecretToken,
) -> Option<SecretToken> {
    let mut displaced = slot.replace(replacement);
    if let Some(token) = displaced.as_mut() {
        token.zeroize();
    }
    displaced
}

pub(crate) fn clear_session_token(slot: &mut Option<SecretToken>) -> Option<SecretToken> {
    let mut displaced = slot.take();
    if let Some(token) = displaced.as_mut() {
        token.zeroize();
    }
    displaced
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::{Signer as _, SigningKey};
    use nwflash_domain::FlashImageInfo;
    use nwflash_infrastructure::{AuthSession, CloudflareClient, SecretToken, DEFAULT_APP_VERSION};
    use nwflash_protection::{
        accept_signed_login_lease, IntegrityProbe, IntegritySignals, LeaseBinding, LeaseClaims,
        LeaseKind, SignedEnvelope, TokenDigest,
    };
    use rand_core::OsRng;
    use wiremock::{matchers::*, Mock, MockServer, Request, ResponseTemplate};
    use zeroize::Zeroizing;

    use super::{
        auth_login_inner, auth_logout_inner, clear_session_token, finalize_login_session,
        replace_session_token, AuthSessionDto,
    };
    use crate::{
        commands::root::RootImageKind,
        exit_supervisor::{ExitPhase, ExitReason, ExitRequest, ExitRequestDisposition},
        AppState, EpochClock, ProtectionTerminalSink, RuntimeProtectionDependencies,
    };

    #[derive(Clone)]
    struct MutableCountingProbe {
        calls: Arc<AtomicUsize>,
        signals: Arc<Mutex<IntegritySignals>>,
    }

    impl MutableCountingProbe {
        fn valid() -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                signals: Arc::new(Mutex::new(IntegritySignals::available(
                    true, true, false, false,
                ))),
            }
        }

        fn invalidate_crc(&self) {
            *self.signals.lock().unwrap() = IntegritySignals::available(true, false, false, false);
        }

        fn count(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl IntegrityProbe for MutableCountingProbe {
        fn signals(&self) -> IntegritySignals {
            self.calls.fetch_add(1, Ordering::AcqRel);
            *self.signals.lock().unwrap()
        }
    }

    struct FixedClock(i64);

    impl EpochClock for FixedClock {
        fn unix_seconds(&self) -> i64 {
            self.0
        }
    }

    #[derive(Default)]
    struct RecordingProtectionSink {
        requests: Mutex<Vec<ExitRequest>>,
    }

    impl ProtectionTerminalSink for RecordingProtectionSink {
        fn request(&self, request: ExitRequest) {
            self.requests.lock().unwrap().push(request);
        }
    }
    fn verified_auth_session(token: &str, session_id: &str) -> AuthSession {
        let signing_key = SigningKey::generate(&mut OsRng);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = LeaseClaims {
            version: 1,
            kind: LeaseKind::Login,
            username: "user".to_string(),
            token_sha256: TokenDigest::sha256(token.as_bytes()),
            client_version: DEFAULT_APP_VERSION.to_string(),
            build_id: "debug-build".to_string(),
            process_nonce: "process-nonce".to_string(),
            session_id: session_id.to_string(),
            sequence: 1,
            issued_at: now,
            expires_at: now + 300,
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signature = URL_SAFE_NO_PAD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
        let envelope = SignedEnvelope {
            lease_payload: payload,
            lease_signature: signature,
        };
        let binding = LeaseBinding::new(
            "user",
            TokenDigest::sha256(token.as_bytes()),
            DEFAULT_APP_VERSION,
            "debug-build",
            "process-nonce",
            session_id,
        );
        let lease = accept_signed_login_lease(
            &envelope,
            &signing_key.verifying_key(),
            &binding,
            now,
        )
        .unwrap();
        AuthSession {
            token: SecretToken::new(token.to_string()),
            username: "user".to_string(),
            name: "User".to_string(),
            lease,
        }
    }

    async fn mount_runtime_signed_login(
        server: &MockServer,
        signing_key: &SigningKey,
        token: &str,
    ) {
        mount_runtime_signed_login_variant(server, signing_key, token, None, None).await;
    }

    async fn mount_runtime_signed_login_variant(
        server: &MockServer,
        signing_key: &SigningKey,
        token: &str,
        response_username: Option<&str>,
        claims_username: Option<&str>,
    ) {
        let signing_key = signing_key.clone();
        let token = token.to_string();
        let response_username = response_username.map(str::to_string);
        let claims_username = claims_username.map(str::to_string);
        Mock::given(method("POST"))
            .and(path("/api/login"))
            .respond_with(move |request: &Request| {
                let body = serde_json::from_slice::<serde_json::Value>(&request.body).unwrap();
                let username = body["username"].as_str().unwrap().to_string();
                let response_username = response_username
                    .clone()
                    .unwrap_or_else(|| username.clone());
                let claims_username = claims_username.clone().unwrap_or_else(|| username.clone());
                let client_version = body["client_version"].as_str().unwrap().to_string();
                let build_id = body["build_id"].as_str().unwrap().to_string();
                let process_nonce = body["process_nonce"].as_str().unwrap().to_string();
                let session_id = body["session_id"].as_str().unwrap().to_string();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64;
                let claims = LeaseClaims {
                    version: 1,
                    kind: LeaseKind::Login,
                    username: claims_username,
                    token_sha256: TokenDigest::sha256(token.as_bytes()),
                    client_version,
                    build_id,
                    process_nonce,
                    session_id,
                    sequence: 1,
                    issued_at: now,
                    expires_at: now + 300,
                };
                let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
                let signature =
                    URL_SAFE_NO_PAD.encode(signing_key.sign(payload.as_bytes()).to_bytes());
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "token": token,
                    "username": response_username,
                    "name": "User",
                    "lease_payload": payload,
                    "lease_signature": signature
                }))
            })
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/heartbeat"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": "transient heartbeat fixture"
            })))
            .mount(server)
            .await;
    }

    #[test]
    fn auth_session_dto_never_serializes_a_bearer_token() {
        let dto = AuthSessionDto {
            username: "user".into(),
            name: "User".into(),
            generation: "generation-test".into(),
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("token"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["generation"],
            "generation-test"
        );
    }

    #[test]
    fn replacement_and_clear_explicitly_zeroize_displaced_owned_tokens() {
        let mut slot = Some(SecretToken::new("old-secret".to_string()));
        let replaced = replace_session_token(&mut slot, SecretToken::new("new-secret".to_string()))
            .expect("replacement should return the displaced token");
        assert!(replaced.is_empty());
        assert_eq!(slot.as_ref().unwrap().as_str(), "new-secret");

        let cleared =
            clear_session_token(&mut slot).expect("clear should return the displaced token");
        assert!(cleared.is_empty());
        assert!(slot.is_none());
    }

    #[tokio::test]
    async fn verified_login_publication_starts_lifecycle_with_the_signed_session() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        mount_runtime_signed_login(&server, &signing_key, "verified-token").await;
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client(client).unwrap();

        let dto = auth_login_inner(
            &state,
            "user".to_string(),
            Zeroizing::new("password".to_string()),
        )
        .await
        .expect("verified idle login should publish");

        let security = state.session_capabilities.security().unwrap();
        assert_eq!(security.lease.sequence(), 1);
        assert_eq!(security.generation, dto.generation);
        assert_eq!(
            state.session_lifecycle.generation().await.as_deref(),
            Some(dto.generation.as_str())
        );
        assert!(state.session_lifecycle.is_running().await);
        assert_eq!(
            state
                .session_token
                .read()
                .unwrap()
                .as_ref()
                .map(SecretToken::as_str),
            Some("verified-token")
        );
        assert_eq!(
            state.exit_supervisor.request(ExitRequest::delayed(
                dto.generation.clone(),
                ExitReason::ServerForced,
            )),
            ExitRequestDisposition::Accepted
        );

        state.session_lifecycle.stop().await.unwrap();
    }

    #[tokio::test]
    async fn verified_login_runs_one_crc_check_before_session_publication() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        mount_runtime_signed_login(&server, &signing_key, "verified-token").await;
        let probe = Arc::new(MutableCountingProbe::valid());
        let sink = Arc::new(RecordingProtectionSink::default());
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client_and_runtime_protection(
            client,
            Arc::new(crate::AppStateTestTerminator::default()),
            RuntimeProtectionDependencies::injected(
                probe.clone(),
                Arc::new(FixedClock(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64,
                )),
                sink,
            ),
        )
        .unwrap();
        assert_eq!(probe.count(), 1);

        auth_login_inner(
            &state,
            "user".to_string(),
            Zeroizing::new("password".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(probe.count(), 2);
        assert!(state.session_capabilities.security().is_ok());
        auth_logout_inner(&state).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_login_crc_preserves_the_existing_session_before_publication() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        mount_runtime_signed_login(&server, &signing_key, "new-token").await;
        let probe = Arc::new(MutableCountingProbe::valid());
        let sink = Arc::new(RecordingProtectionSink::default());
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client_and_runtime_protection(
            client,
            Arc::new(crate::AppStateTestTerminator::default()),
            RuntimeProtectionDependencies::injected(
                probe.clone(),
                Arc::new(FixedClock(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64,
                )),
                sink.clone(),
            ),
        )
        .unwrap();
        finalize_login_session(
            &state,
            verified_auth_session("old-token", "old-signed-session"),
            "generation-old".to_string(),
        )
        .await
        .unwrap();
        probe.invalidate_crc();

        let result = auth_login_inner(
            &state,
            "user".to_string(),
            Zeroizing::new("password".to_string()),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(probe.count(), 2);
        assert_eq!(
            state
                .session_token
                .read()
                .unwrap()
                .as_ref()
                .map(SecretToken::as_str),
            Some("old-token")
        );
        assert_eq!(
            state.session_capabilities.security().unwrap().generation,
            "generation-old"
        );
        {
            let requests = sink.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].phase(), ExitPhase::Login);
        }
        auth_logout_inner(&state).await.unwrap();
    }

    #[tokio::test]
    async fn explicit_logout_clears_only_the_published_supervisor_generation() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        mount_runtime_signed_login(&server, &signing_key, "logout-token").await;
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client(client).unwrap();
        let session = auth_login_inner(
            &state,
            "user".to_string(),
            Zeroizing::new("password".to_string()),
        )
        .await
        .expect("signed login should publish its generation");

        auth_logout_inner(&state)
            .await
            .expect("explicit logout should clear the session");

        assert_eq!(
            state.exit_supervisor.request(ExitRequest::delayed(
                session.generation,
                ExitReason::ServerForced,
            )),
            ExitRequestDisposition::IgnoredStaleGeneration
        );
        assert_eq!(
            state.operation_coordinator.admission_state(),
            nwflash_application::OperationAdmissionState::Running
        );
    }

    #[tokio::test]
    async fn unsigned_login_response_publishes_no_token_capability_or_lifecycle() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        Mock::given(method("POST"))
            .and(path("/api/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "unverified-token",
                "username": "user",
                "name": "User",
                "lease_payload": "",
                "lease_signature": ""
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/heartbeat"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": "transient heartbeat fixture"
            })))
            .mount(&server)
            .await;
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client(client).unwrap();

        let result = auth_login_inner(
            &state,
            "user".to_string(),
            Zeroizing::new("password".to_string()),
        )
        .await;

        assert!(result.is_err());
        assert!(state.session_token.read().unwrap().is_none());
        assert!(state.session_capabilities.capture().is_err());
        assert!(!state.session_lifecycle.is_running().await);
    }

    #[tokio::test]
    async fn server_error_echoes_never_cross_the_auth_webview_boundary() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        Mock::given(method("POST"))
            .and(path("/api/login"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "echo-token echo-password C:\\private\\firmware.img"
            })))
            .mount(&server)
            .await;
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client(client).unwrap();

        let result = auth_login_inner(
            &state,
            "user".to_string(),
            Zeroizing::new("echo-password".to_string()),
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("server rejection should reach the fixed user boundary"),
        };

        assert!(!error.contains("echo-token"));
        assert!(!error.contains("echo-password"));
        assert!(!error.contains("private"));
        assert!(!error.contains("firmware.img"));
        assert_eq!(error, "用户名或密码错误，或账号不可用。");
    }

    #[tokio::test]
    async fn unsigned_replacement_preserves_the_existing_verified_session() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        Mock::given(method("POST"))
            .and(path("/api/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token": "unverified-token",
                "username": "user",
                "name": "User",
                "lease_payload": "",
                "lease_signature": ""
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/heartbeat"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": "transient heartbeat fixture"
            })))
            .mount(&server)
            .await;
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client(client).unwrap();
        finalize_login_session(
            &state,
            verified_auth_session("old-token", "old-signed-session"),
            "generation-old".to_string(),
        )
        .await
        .unwrap();

        let result = auth_login_inner(
            &state,
            "user".to_string(),
            Zeroizing::new("password".to_string()),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            state
                .session_token
                .read()
                .unwrap()
                .as_ref()
                .map(SecretToken::as_str),
            Some("old-token")
        );
        assert_eq!(
            state
                .session_capabilities
                .security()
                .unwrap()
                .lease
                .session_id(),
            "old-signed-session"
        );
        assert_eq!(
            state.session_lifecycle.session_id().await.as_deref(),
            Some("old-signed-session")
        );
        assert!(state.session_lifecycle.is_running().await);
        state.session_lifecycle.stop().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_signed_token_or_username_preserves_old_session_and_artifact() {
        let cases = [
            ("unsafe-token", "new-token\ninvalid", None, None),
            (
                "wrong-username",
                "new-token",
                Some("other-user"),
                Some("other-user"),
            ),
        ];

        for (label, token, response_username, claims_username) in cases {
            let server = MockServer::start().await;
            let signing_key = SigningKey::generate(&mut OsRng);
            mount_runtime_signed_login_variant(
                &server,
                &signing_key,
                token,
                response_username,
                claims_username,
            )
            .await;
            let client = CloudflareClient::new_injected_with_lease_key(
                server.uri(),
                DEFAULT_APP_VERSION,
                signing_key.verifying_key(),
            );
            let state = AppState::try_with_client(client).unwrap();
            finalize_login_session(
                &state,
                verified_auth_session("old-token", "old-signed-session"),
                "generation-old".to_string(),
            )
            .await
            .unwrap();
            let root = std::env::temp_dir().join(format!(
                "nwflash-auth-rollback-{label}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&root).unwrap();
            let image = root.join("external-init_boot.img");
            fs::write(&image, b"external").unwrap();
            let capability = state.session_capabilities.capture().unwrap();
            let selection = state
                .root_image_runtime
                .replace_with_target(
                    capability,
                    RootImageKind::InitBoot,
                    FlashImageInfo {
                        path: image.to_string_lossy().into_owned(),
                        size_bytes: 8,
                    },
                    "init_boot".to_string(),
                )
                .unwrap();

            let result = auth_login_inner(
                &state,
                "user".to_string(),
                Zeroizing::new("password".to_string()),
            )
            .await;

            assert!(result.is_err(), "{label}");
            assert_eq!(
                state
                    .session_token
                    .read()
                    .unwrap()
                    .as_ref()
                    .map(SecretToken::as_str),
                Some("old-token"),
                "{label}"
            );
            assert_eq!(
                state.session_lifecycle.generation().await.as_deref(),
                Some("generation-old"),
                "{label}"
            );
            assert!(state.session_capabilities.is_current(capability), "{label}");
            assert!(
                state
                    .root_image_runtime
                    .get(RootImageKind::InitBoot, &selection.id)
                    .is_ok(),
                "{label}"
            );

            state.session_lifecycle.stop().await.unwrap();
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_prepared_start_input_preserves_old_session_and_artifact() {
        let server = MockServer::start().await;
        let signing_key = SigningKey::generate(&mut OsRng);
        Mock::given(method("POST"))
            .and(path("/api/heartbeat"))
            .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
                "error": "transient heartbeat fixture"
            })))
            .mount(&server)
            .await;
        let client = CloudflareClient::new_injected_with_lease_key(
            server.uri(),
            DEFAULT_APP_VERSION,
            signing_key.verifying_key(),
        );
        let state = AppState::try_with_client(client).unwrap();
        finalize_login_session(
            &state,
            verified_auth_session("old-token", "old-signed-session"),
            "generation-old".to_string(),
        )
        .await
        .unwrap();
        let root = std::env::temp_dir().join(format!(
            "nwflash-auth-start-rollback-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let image = root.join("external-init_boot.img");
        fs::write(&image, b"external").unwrap();
        let capability = state.session_capabilities.capture().unwrap();
        let selection = state
            .root_image_runtime
            .replace_with_target(
                capability,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: image.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .unwrap();

        let result = finalize_login_session(
            &state,
            verified_auth_session("new-token", "new-signed-session"),
            String::new(),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(
            state
                .session_token
                .read()
                .unwrap()
                .as_ref()
                .map(SecretToken::as_str),
            Some("old-token")
        );
        assert_eq!(
            state.session_lifecycle.generation().await.as_deref(),
            Some("generation-old")
        );
        assert!(state.session_capabilities.is_current(capability));
        assert!(state
            .root_image_runtime
            .get(RootImageKind::InitBoot, &selection.id)
            .is_ok());

        state.session_lifecycle.stop().await.unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
