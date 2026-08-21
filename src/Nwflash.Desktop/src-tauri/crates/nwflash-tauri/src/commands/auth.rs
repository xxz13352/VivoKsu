use serde::Serialize;
use tauri::State;

use crate::AppState;
use nwflash_application::{SessionLifecycleError, OPERATION_IN_PROGRESS_MESSAGE};

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
    let session = state
        .auth_service
        .login(&username, &password)
        .await
        .map_err(|error| error.to_string())?;

    finalize_login_session(&state, session.token.clone()).await?;

    Ok(AuthSessionDto {
        username: session.username,
        name: session.name,
    })
}

async fn finalize_login_session(state: &AppState, new_token: String) -> Result<(), String> {
    let idle_lease = state
        .operation_coordinator
        .try_acquire_idle()
        .map_err(|_| OPERATION_IN_PROGRESS_MESSAGE.to_string())?;
    state.revoke_root_capabilities(&idle_lease);

    match state.session_lifecycle.stop().await {
        Ok(()) | Err(SessionLifecycleError::NotStarted) => {}
        Err(error) => return Err(error.to_string()),
    }
    state.usage_reporter.flush().await;

    let mut token = state
        .session_token
        .write()
        .expect("session token lock should not be poisoned");
    *token = Some(new_token);
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
    state.revoke_root_capabilities(&idle_lease);

    match state.session_lifecycle.stop().await {
        Ok(()) | Err(SessionLifecycleError::NotStarted) => {}
        Err(error) => return Err(error.to_string()),
    }
    state.usage_reporter.flush().await;

    let mut token = state
        .session_token
        .write()
        .expect("session token lock should not be poisoned");
    token.take();
    Ok(LogoutResult { ok: true })
}

#[tauri::command]
pub async fn auth_validate_token(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let token = state
        .session_token
        .read()
        .expect("session token lock should not be poisoned")
        .clone();

    match token {
        Some(token) => state
            .auth_service
            .validate_token(&token)
            .await
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, time::Duration};

    use super::*;
    use crate::commands::root::RootImageKind;
    use futures::future::BoxFuture;
    use nwflash_application::{
        OperationCoordinator, SessionLifecycle, OPERATION_IN_PROGRESS_MESSAGE,
    };
    use nwflash_domain::FlashImageInfo;
    use nwflash_infrastructure::api_client::{CloudflareError, HeartbeatResult};

    fn app_state_with_local_session() -> AppState {
        let mut state = AppState::new();
        let heartbeat = Arc::new(|_token: String, _session_id: String, _active: bool| {
            let future: BoxFuture<'static, Result<HeartbeatResult, CloudflareError>> =
                Box::pin(async {
                    Ok(HeartbeatResult {
                        force_exit: false,
                        reason: None,
                    })
                });
            future
        });
        state.session_lifecycle = SessionLifecycle::with_intervals(
            heartbeat,
            None,
            None,
            Duration::from_millis(10),
            Duration::from_millis(100),
            Duration::from_millis(100),
        );
        state.operation_coordinator = OperationCoordinator::default();
        state
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

    #[tokio::test]
    async fn direct_logout_revokes_root_capability_before_clearing_token() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("token".to_string());
        state
            .session_lifecycle
            .start("session-1".to_string(), "token".to_string())
            .await
            .expect("test session should start");
        let lease = state.session_capabilities.activate();
        let root = std::env::temp_dir().join(format!(
            "nwflash-auth-logout-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("logout fixture should be created");
        let external_image = root.join("external-init_boot.img");
        fs::write(&external_image, b"external").expect("external image fixture");
        let selection = state
            .root_image_runtime
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: external_image.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .expect("current logout fixture should publish a ROOT image");
        let result = auth_logout_inner(&state).await;
        let lifecycle_running = state.session_lifecycle.is_running().await;

        assert!(result.expect("idle logout should succeed").ok);
        assert!(state.session_capabilities.capture().is_err());
        assert!(state
            .root_image_runtime
            .get(RootImageKind::InitBoot, &selection.id)
            .is_err());
        assert!(external_image.exists());
        assert!(!lifecycle_running);
        assert!(state
            .session_token
            .read()
            .expect("session token lock should be healthy")
            .is_none());
        fs::remove_dir_all(root).expect("logout fixture should be removed");
    }

    #[tokio::test]
    async fn busy_logout_preserves_token_and_root_capability() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("token".to_string());
        let capability = state.session_capabilities.activate();
        let busy_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("test should hold operation admission");
        let result = auth_logout_inner(&state).await;
        let token = state
            .session_token
            .read()
            .expect("session token lock should be healthy")
            .clone();
        let capability_preserved = state.session_capabilities.is_current(capability);
        drop(busy_lease);

        assert_eq!(
            result.err().expect("busy logout should fail"),
            OPERATION_IN_PROGRESS_MESSAGE
        );
        assert_eq!(token.as_deref(), Some("token"));
        assert!(capability_preserved);
    }

    #[tokio::test]
    async fn busy_login_replacement_preserves_the_existing_session() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("old-token".to_string());
        state
            .session_lifecycle
            .start("session-1".to_string(), "old-token".to_string())
            .await
            .expect("test session should start");
        let lease = state.session_capabilities.activate();
        let root = std::env::temp_dir().join(format!(
            "nwflash-auth-login-busy-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("login fixture should be created");
        let external_image = root.join("external-init_boot.img");
        fs::write(&external_image, b"external").expect("external image fixture");
        let selection = state
            .root_image_runtime
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: external_image.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .expect("current login fixture should publish a ROOT image");
        let busy_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("test should hold operation admission");

        let result = finalize_login_session(&state, "new-token".to_string()).await;
        let token = state
            .session_token
            .read()
            .expect("session token lock should be healthy")
            .clone();
        let root_capability_remains_readable = state
            .root_image_runtime
            .get(RootImageKind::InitBoot, &selection.id)
            .is_ok();
        let lifecycle_running = state.session_lifecycle.is_running().await;

        drop(busy_lease);
        state
            .session_lifecycle
            .stop()
            .await
            .expect("test session should stop");
        fs::remove_dir_all(root).expect("login fixture should be removed");

        assert_eq!(
            result.expect_err("busy login replacement should fail"),
            OPERATION_IN_PROGRESS_MESSAGE
        );
        assert_eq!(token.as_deref(), Some("old-token"));
        assert!(root_capability_remains_readable);
        assert!(lifecycle_running);
    }

    #[tokio::test]
    async fn idle_login_replacement_revokes_the_existing_session_before_storing_the_new_token() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("old-token".to_string());
        state
            .session_lifecycle
            .start("session-1".to_string(), "old-token".to_string())
            .await
            .expect("test session should start");
        let lease = state.session_capabilities.activate();
        let root = std::env::temp_dir().join(format!(
            "nwflash-auth-login-idle-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("login fixture should be created");
        let external_image = root.join("external-init_boot.img");
        fs::write(&external_image, b"external").expect("external image fixture");
        let selection = state
            .root_image_runtime
            .replace_with_target(
                lease,
                RootImageKind::InitBoot,
                FlashImageInfo {
                    path: external_image.to_string_lossy().into_owned(),
                    size_bytes: 8,
                },
                "init_boot".to_string(),
            )
            .expect("current login fixture should publish a ROOT image");

        let result = finalize_login_session(&state, "new-token".to_string()).await;
        let token = state
            .session_token
            .read()
            .expect("session token lock should be healthy")
            .clone();
        let old_root_capability_is_invalid = state
            .root_image_runtime
            .get(RootImageKind::InitBoot, &selection.id)
            .is_err();
        let lifecycle_running = state.session_lifecycle.is_running().await;

        fs::remove_dir_all(root).expect("login fixture should be removed");

        assert!(result.is_ok());
        assert_eq!(token.as_deref(), Some("new-token"));
        assert!(old_root_capability_is_invalid);
        assert!(!lifecycle_running);
    }
}
