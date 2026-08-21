use tauri::State;

#[tauri::command]
pub async fn operation_cancel(state: State<'_, crate::AppState>) -> Result<(), String> {
    state.operation_coordinator.cancel_current().await;
    Ok(())
}
