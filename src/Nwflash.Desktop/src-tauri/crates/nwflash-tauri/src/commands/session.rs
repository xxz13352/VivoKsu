use serde::Serialize;
use tauri::State;

use crate::{commands::auth::clear_session_token, AppState};
use nwflash_application::OPERATION_IN_PROGRESS_MESSAGE;

#[derive(Debug, Serialize)]
pub struct SessionState {
    pub running: bool,
    pub healthy: bool,
    pub session_id: Option<String>,
    pub has_token: bool,
}

#[tauri::command]
pub async fn session_start(state: State<'_, AppState>) -> Result<SessionState, String> {
    session_start_inner(&state).await
}

async fn session_start_inner(_state: &AppState) -> Result<SessionState, String> {
    Err("会话只能由已验证的 Rust 登录流程启动。".to_string())
}

#[tauri::command]
pub async fn session_stop(state: State<'_, AppState>) -> Result<SessionState, String> {
    session_stop_inner(&state).await
}

async fn session_stop_inner(state: &AppState) -> Result<SessionState, String> {
    let idle_lease = state
        .operation_coordinator
        .try_acquire_idle()
        .map_err(|_| OPERATION_IN_PROGRESS_MESSAGE.to_string())?;
    state
        .session_lifecycle
        .stop()
        .await
        .map_err(|error| error.to_string())?;
    state.usage_reporter.flush().await;
    state.revoke_root_capabilities(&idle_lease);
    {
        let mut token = state
            .session_token
            .write()
            .expect("session token lock should not be poisoned");
        let _ = clear_session_token(&mut token);
    }
    Ok(read_session_state(state).await)
}

#[tauri::command]
pub async fn session_state(state: State<'_, AppState>) -> Result<SessionState, String> {
    Ok(read_session_state(&state).await)
}

async fn read_session_state(state: &AppState) -> SessionState {
    SessionState {
        running: state.session_lifecycle.is_running().await,
        healthy: state.session_lifecycle.is_healthy(),
        session_id: state.session_lifecycle.session_id().await,
        has_token: state
            .session_token
            .read()
            .expect("session token lock should not be poisoned")
            .is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::session_start_inner;
    use crate::AppState;

    #[tokio::test]
    async fn retained_session_start_cannot_activate_without_a_signed_rust_login() {
        let state = AppState::new();

        let result = session_start_inner(&state).await;

        assert_eq!(
            result.expect_err("frontend session start must be rejected"),
            "会话只能由已验证的 Rust 登录流程启动。"
        );
        assert!(state.session_capabilities.capture().is_err());
        assert!(state.session_token.read().unwrap().is_none());
        assert!(!state.session_lifecycle.is_running().await);
    }
}
