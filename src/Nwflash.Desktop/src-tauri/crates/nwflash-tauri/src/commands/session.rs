use std::future::Future;

use serde::Serialize;
use tauri::State;

use crate::AppState;
use nwflash_application::{SessionLifecycleError, OPERATION_IN_PROGRESS_MESSAGE};

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
    session_start_inner(&state, session_id).await
}

async fn session_start_inner(state: &AppState, session_id: String) -> Result<SessionState, String> {
    session_start_inner_with_hook(state, session_id, || async {}).await
}

async fn session_start_inner_with_hook<Hook, HookFuture>(
    state: &AppState,
    session_id: String,
    after_lifecycle_start: Hook,
) -> Result<SessionState, String>
where
    Hook: FnOnce() -> HookFuture,
    HookFuture: Future<Output = ()>,
{
    let idle_lease = state
        .operation_coordinator
        .try_acquire_idle()
        .map_err(|_| OPERATION_IN_PROGRESS_MESSAGE.to_string())?;
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
    after_lifecycle_start().await;
    state.session_capabilities.activate();
    drop(idle_lease);
    state.usage_reporter.start_if_needed();
    Ok(read_session_state(state).await)
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
    state.revoke_root_capabilities(&idle_lease);
    state
        .session_lifecycle
        .stop()
        .await
        .map_err(map_session_error)?;
    state.usage_reporter.flush().await;
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

fn map_session_error(error: SessionLifecycleError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, time::Duration};

    use super::{session_start_inner, session_start_inner_with_hook, session_stop_inner};
    use crate::{commands::root::RootImageKind, AppState};
    use futures::future::BoxFuture;
    use nwflash_application::{
        OperationCoordinator, OperationCoordinatorError, SessionLifecycle,
        OPERATION_IN_PROGRESS_MESSAGE,
    };
    use nwflash_domain::FlashImageInfo;
    use nwflash_infrastructure::api_client::{CloudflareError, HeartbeatResult};
    use tokio::sync::Notify;

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

    #[tokio::test]
    async fn successful_session_start_activates_root_capabilities() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("token".to_string());
        assert!(state.session_capabilities.capture().is_err());

        let started = session_start_inner(&state, "session-1".to_string()).await;
        let capability_after_start = state.session_capabilities.capture();
        state
            .session_lifecycle
            .stop()
            .await
            .expect("test session should stop");

        assert!(started.is_ok());
        assert!(capability_after_start.is_ok());
    }

    #[tokio::test]
    async fn session_start_holds_idle_lease_from_lifecycle_start_through_activation() {
        let state = Arc::new(app_state_with_local_session());
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("token".to_string());
        let start_paused = Arc::new(Notify::new());
        let release_start = Arc::new(Notify::new());
        let starting = tokio::spawn({
            let state = state.clone();
            let start_paused = start_paused.clone();
            let release_start = release_start.clone();
            async move {
                session_start_inner_with_hook(&state, "session-1".to_string(), || async move {
                    start_paused.notify_one();
                    release_start.notified().await;
                })
                .await
            }
        });
        start_paused.notified().await;

        let teardown_attempt = state.operation_coordinator.try_acquire_idle();
        let teardown_blocked =
            matches!(teardown_attempt, Err(OperationCoordinatorError::InProgress));
        if let Ok(teardown_lease) = teardown_attempt {
            state.revoke_root_capabilities(&teardown_lease);
        }
        release_start.notify_one();
        let start_result = starting.await.expect("start task should join");
        let capability_active = state.session_capabilities.capture().is_ok();
        if state.session_lifecycle.is_running().await {
            state
                .session_lifecycle
                .stop()
                .await
                .expect("test session should stop");
        }

        assert!(teardown_blocked);
        assert!(start_result.is_ok());
        assert!(capability_active);
    }

    #[tokio::test]
    async fn busy_session_start_rejects_without_starting_or_activating() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("token".to_string());
        let busy_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("test should hold operation admission");

        let result = session_start_inner(&state, "session-1".to_string()).await;
        let lifecycle_running = state.session_lifecycle.is_running().await;
        let capability_active = state.session_capabilities.capture().is_ok();
        drop(busy_lease);
        if lifecycle_running {
            state
                .session_lifecycle
                .stop()
                .await
                .expect("test session cleanup should stop");
        }
        if capability_active {
            state.session_capabilities.invalidate(|| {});
        }

        assert_eq!(
            result.err().expect("busy session start should fail"),
            OPERATION_IN_PROGRESS_MESSAGE
        );
        assert!(!lifecycle_running);
        assert!(!capability_active);
    }

    #[tokio::test]
    async fn failed_session_start_leaves_root_capabilities_inactive() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("token".to_string());

        let result = session_start_inner(&state, " ".to_string()).await;

        assert!(result.is_err());
        assert!(state.session_capabilities.capture().is_err());
        assert!(!state.session_lifecycle.is_running().await);
    }

    #[tokio::test]
    async fn successful_session_stop_invalidates_root_id_without_deleting_external_image() {
        let state = app_state_with_local_session();
        *state
            .session_token
            .write()
            .expect("session token lock should be healthy") = Some("token".to_string());
        session_start_inner(&state, "session-1".to_string())
            .await
            .expect("test session should start");
        let lease = state
            .session_capabilities
            .capture()
            .expect("started session should expose ROOT capability");
        let root = std::env::temp_dir().join(format!(
            "nwflash-session-stop-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock should be after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("session stop fixture should be created");
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
            .expect("current session should publish the external image selection");

        let stopped = session_stop_inner(&state)
            .await
            .expect("idle session stop should succeed");

        assert!(!stopped.running);
        assert!(state.session_capabilities.capture().is_err());
        assert!(state
            .root_image_runtime
            .get(RootImageKind::InitBoot, &selection.id)
            .is_err());
        assert!(external_image.exists());
        fs::remove_dir_all(root).expect("session stop fixture should be removed");
    }

    #[tokio::test]
    async fn busy_session_stop_preserves_lifecycle_token_and_root_capability() {
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
        let capability = state.session_capabilities.activate();
        let busy_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("test should hold operation admission");
        let result = session_stop_inner(&state).await;
        let lifecycle_running = state.session_lifecycle.is_running().await;
        let token = state
            .session_token
            .read()
            .expect("session token lock should be healthy")
            .clone();
        let capability_preserved = state.session_capabilities.is_current(capability);
        drop(busy_lease);
        if lifecycle_running {
            state
                .session_lifecycle
                .stop()
                .await
                .expect("test session cleanup should stop");
        }

        assert_eq!(
            result.err().expect("busy session stop should fail"),
            OPERATION_IN_PROGRESS_MESSAGE
        );
        assert!(lifecycle_running);
        assert_eq!(token.as_deref(), Some("token"));
        assert!(capability_preserved);
    }
}
