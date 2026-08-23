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
    let session = state
        .auth_service
        .login(
            &username,
            password.as_str(),
            &state.process_identity,
            &session_id,
        )
        .await
        .map_err(|error| error.to_string())?;
    let dto = AuthSessionDto {
        username: session.username.clone(),
        name: session.name.clone(),
    };

    finalize_login_session(state, session).await?;
    Ok(dto)
}

async fn finalize_login_session(state: &AppState, session: AuthSession) -> Result<(), String> {
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

    let lifecycle_session = SessionLifecycleSession::new(
        session.token.request_scope(),
        session.username.clone(),
        session.lease.clone(),
    );
    let capability = state
        .session_capabilities
        .activate_verified(session.username, session.lease);
    {
        let mut token = state
            .session_token
            .write()
            .expect("session token lock should not be poisoned");
        let _ = replace_session_token(&mut token, session.token);
    }

    if let Err(error) = state.session_lifecycle.start(lifecycle_session).await {
        state.revoke_root_capabilities(&idle_lease);
        let mut token = state
            .session_token
            .write()
            .expect("session token lock should not be poisoned");
        let _ = clear_session_token(&mut token);
        return Err(error.to_string());
    }
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
    let mut token = state
        .session_token
        .write()
        .expect("session token lock should not be poisoned");
    let _ = clear_session_token(&mut token);
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
            .map_err(|error| error.to_string()),
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use ed25519_dalek::{Signer as _, SigningKey};
    use nwflash_infrastructure::{AuthSession, CloudflareClient, SecretToken, DEFAULT_APP_VERSION};
    use nwflash_protection::{
        accept_login_lease, verify_signed_lease, LeaseBinding, LeaseClaims, LeaseKind,
        SignedEnvelope, TokenDigest,
    };
    use rand_core::OsRng;
    use wiremock::{matchers::*, Mock, MockServer, ResponseTemplate};
    use zeroize::Zeroizing;

    use super::{
        auth_login_inner, clear_session_token, finalize_login_session, replace_session_token,
        AuthSessionDto,
    };
    use crate::AppState;

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
        let verified = verify_signed_lease(
            &SignedEnvelope {
                lease_payload: payload,
                lease_signature: signature,
            },
            &signing_key.verifying_key(),
        )
        .unwrap();
        let binding = LeaseBinding::new(
            "user",
            TokenDigest::sha256(token.as_bytes()),
            DEFAULT_APP_VERSION,
            "debug-build",
            "process-nonce",
            session_id,
        );
        let lease = accept_login_lease(&verified, &binding, now).unwrap();
        AuthSession {
            token: SecretToken::new(token.to_string()),
            username: "user".to_string(),
            name: "User".to_string(),
            lease,
        }
    }

    #[test]
    fn auth_session_dto_never_serializes_a_bearer_token() {
        let dto = AuthSessionDto {
            username: "user".into(),
            name: "User".into(),
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("token"));
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
        let state = AppState::new();
        let session = verified_auth_session("verified-token", "signed-session");

        finalize_login_session(&state, session)
            .await
            .expect("verified idle login should publish");

        let security = state.session_capabilities.security().unwrap();
        assert_eq!(security.lease.session_id(), "signed-session");
        assert_eq!(security.lease.sequence(), 1);
        assert_eq!(
            state.session_lifecycle.session_id().await.as_deref(),
            Some("signed-session")
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

        state.session_lifecycle.stop().await.unwrap();
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
}
