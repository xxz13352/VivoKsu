use serde::Serialize;
use tauri::State;

use crate::AppState;
use nwflash_application::SessionLifecycleError;

#[derive(Serialize)]
pub struct SessionState {
    pub running: bool,
    pub healthy: bool,
    pub session_id: Option<String>,
    pub has_token: bool,
}

#[tauri::command]
pub async fn session_start(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SessionState, String> {
    let token = {
        state
            .session_token
            .read()
            .expect("session token lock should not be poisoned")
            .clone()
    };

    let token = token.ok_or_else(|| "未登录，无法启动会话。".to_string())?;
    state
        .session_lifecycle
        .start(session_id, token)
        .await
        .map_err(map_session_error)?;
    state.usage_reporter.start_if_needed();
    Ok(read_session_state(&state).await)
}

#[tauri::command]
pub async fn session_stop(state: State<'_, AppState>) -> Result<SessionState, String> {
    state
        .session_lifecycle
        .stop()
        .await
        .map_err(map_session_error)?;
    state.usage_reporter.flush().await;
    // 会话结束：清理 ROOT 云提取缓存与 staging，避免跨会话残留私密 OTA 数据。
    state.root_ota_runtime.cleanup();
    Ok(read_session_state(&state).await)
}

#[tauri::command]
pub async fn session_state(state: State<'_, AppState>) -> Result<SessionState, String> {
    Ok(read_session_state(&state).await)
}

async fn read_session_state(state: &State<'_, AppState>) -> SessionState {
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

fn map_session_error(error: SessionLifecycleError) -> String {
    error.to_string()
}
