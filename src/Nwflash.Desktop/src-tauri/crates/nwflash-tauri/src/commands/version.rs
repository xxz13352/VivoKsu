use crate::AppState;
use crate::VersionCheckResponse;
use tauri::State;

#[tauri::command]
pub async fn version_check(state: State<'_, AppState>) -> Result<VersionCheckResponse, String> {
    Ok(state.version_client.check().await.into())
}
