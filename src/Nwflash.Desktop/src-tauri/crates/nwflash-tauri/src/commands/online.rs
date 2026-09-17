use serde::Serialize;
use tauri::State;

use crate::AppState;
use nwflash_infrastructure::{OnlineSession, SecretToken};

#[derive(Serialize)]
pub struct OnlineSessionDto {
    pub name: String,
    pub client_version: String,
    pub connected_at: i64,
    pub last_seen_at: i64,
    pub duration_seconds: i64,
    pub is_self: bool,
}

#[tauri::command]
pub async fn online_sessions(state: State<'_, AppState>) -> Result<Vec<OnlineSessionDto>, String> {
    let token = state
        .session_token
        .read()
        .expect("session token lock should not be poisoned")
        .as_ref()
        .map(SecretToken::request_scope)
        .ok_or_else(|| "未登录，无法获取在线会话列表。".to_string())?;

    state
        .client
        .get_online(token.as_str())
        .await
        .map(OnlineSessionDto::from_model_list)
        .map_err(|error| error.user_message())
}

impl OnlineSessionDto {
    fn from_model(session: OnlineSession) -> Self {
        Self {
            name: session.name,
            client_version: session.client_version,
            connected_at: session.connected_at,
            last_seen_at: session.last_seen_at,
            duration_seconds: session.duration_seconds,
            is_self: session.is_self,
        }
    }

    fn from_model_list(sessions: Vec<OnlineSession>) -> Vec<Self> {
        sessions.into_iter().map(Self::from_model).collect()
    }
}
