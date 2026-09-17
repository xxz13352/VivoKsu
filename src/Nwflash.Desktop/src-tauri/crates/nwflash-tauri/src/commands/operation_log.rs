//! Tauri commands for operation logs.

use tauri::State;

use crate::AppState;
use nwflash_domain::OperationLogEntry;

#[tauri::command]
pub fn operation_logs_snapshot(state: State<'_, AppState>) -> Vec<OperationLogEntry> {
    state.operation_log_store.snapshot()
}

#[tauri::command]
pub fn operation_logs_clear(state: State<'_, AppState>) {
    state.operation_log_store.clear_memory();
}
