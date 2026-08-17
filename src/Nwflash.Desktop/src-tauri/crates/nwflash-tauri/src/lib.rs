use std::{
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use nwflash_application::{
    OperationAuthorization, OperationCoordinator, OperationLogger, OperationPermissionGate,
    SessionLifecycle,
};
use nwflash_domain::{DomainError, OperationKind};
use nwflash_infrastructure::api_client::{CloudflareError, HeartbeatResult};
use nwflash_infrastructure::{
    api_client::UpdateRequiredInfo, AuthService, CloudflareClient, OperationLogStore,
    VersionCheckResult, VersionClient,
};
use serde::Serialize;
use tauri::{async_runtime::spawn, AppHandle, Emitter, Manager, Wry};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::time::sleep;

mod commands;

#[doc(hidden)]
pub use commands::mirror::{start_plan, MirrorRuntime};
mod usage_reporter;

pub const APP_LABEL: &str = "奶蛙Flash";
const SESSION_FORCE_EXIT_EVENT: &str = "session:force-exit";
const SESSION_UPDATE_REQUIRED_EVENT: &str = "session:update-required";
const SESSION_SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
enum SessionLifecycleEvent {
    ForceExit(String),
    UpdateRequired(UpdateRequiredInfo),
}

pub struct AppState {
    pub client: CloudflareClient,
    pub auth_service: AuthService,
    pub version_client: VersionClient,
    pub session_token: Arc<RwLock<Option<String>>>,
    pub usage_reporter: Arc<usage_reporter::UsageLogReporter>,
    pub session_lifecycle: SessionLifecycle,
    pub operation_coordinator: OperationCoordinator,
    pub device_runtime: commands::device::DeviceRuntime,
    pub firmware_artifacts: commands::firmware::FirmwareArtifactRuntime,
    pub firmware_extraction: commands::firmware::FirmwareExtractionRuntime,
    pub payload_inspection: commands::firmware::PayloadInspectionRuntime,
    pub(crate) firmware_progress: commands::firmware::FirmwareProgressRuntime,
    pub prepared_firmware_artifact: commands::quick_flash::PreparedFirmwareArtifactRuntime,
    pub prepared_dual_slot: commands::quick_flash::PreparedDualSlotRuntime,
    pub partition_workspace: commands::partitions::PartitionWorkspaceRuntime,
    pub mirror_runtime: commands::mirror::MirrorRuntime,
    pub root_image_runtime: commands::root::RootImageRuntime,
    pub root_patched_artifacts: commands::root::RootPatchedArtifactRuntime,
    pub root_ota_runtime: commands::root_ota::RootOtaRuntime,
    pub safe_flash_runtime: commands::safe_flash::SafeFlashRuntime,
    pub(crate) session_events_rx: Mutex<Option<UnboundedReceiver<SessionLifecycleEvent>>>,
    pub(crate) operation_log_store: Arc<OperationLogStore>,
}

#[derive(Clone)]
struct CloudflareOperationPermissionGate {
    client: CloudflareClient,
    session_token: Arc<RwLock<Option<String>>>,
}

impl CloudflareOperationPermissionGate {
    fn new(client: CloudflareClient, session_token: Arc<RwLock<Option<String>>>) -> Self {
        Self {
            client,
            session_token,
        }
    }
}

impl OperationPermissionGate for CloudflareOperationPermissionGate {
    fn authorize(
        &self,
        operation: OperationKind,
        title: String,
    ) -> futures::future::BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
        let client = self.client.clone();
        let session_token = self.session_token.clone();
        Box::pin(async move {
            let token = session_token
                .read()
                .expect("session token lock should not be poisoned")
                .clone()
                .filter(|token| !token.is_empty())
                .ok_or_else(|| {
                    DomainError::AuthorizationDenied("未登录，无法执行受控操作。".to_string())
                })?;
            let authorization = client
                .authorize_operation(&token, &format!("{operation:?}"), &title)
                .await
                .map_err(|error| DomainError::RemoteApi(error.to_string()))?;
            Ok(OperationAuthorization {
                allowed: authorization.allowed,
                reason: authorization.reason,
            })
        })
    }
}

impl AppState {
    pub fn new() -> Self {
        let client = CloudflareClient::new_default();
        let (session_events_tx, session_events_rx) = unbounded_channel();
        let session_token = Arc::new(RwLock::new(None));
        let permission_gate = Arc::new(CloudflareOperationPermissionGate::new(
            client.clone(),
            session_token.clone(),
        ));

        let heartbeat_fn = {
            let heartbeat_client = client.clone();
            std::sync::Arc::new(move |token: String, session_id: String, active: bool| {
                let heartbeat_client = heartbeat_client.clone();
                let token = token;
                let session_id = session_id;
                let future: futures::future::BoxFuture<
                    'static,
                    Result<HeartbeatResult, CloudflareError>,
                > = Box::pin(async move {
                    heartbeat_client
                        .heartbeat(&token, &session_id, active)
                        .await
                });
                future
            })
        };

        let tx_force_exit = session_events_tx.clone();
        let on_force_exit = std::sync::Arc::new(move |reason: String| {
            let _ = tx_force_exit.send(SessionLifecycleEvent::ForceExit(reason));
        });
        let tx_update_required = session_events_tx.clone();
        let on_update_required = std::sync::Arc::new(move |update: UpdateRequiredInfo| {
            let _ = tx_update_required.send(SessionLifecycleEvent::UpdateRequired(update));
        });

        let operation_log_store = Arc::new(OperationLogStore::with_default_path(500));
        let operation_log_buffer = Arc::new(OperationLogBuffer {
            entries: operation_log_store.clone(),
        });
        let usage_reporter =
            usage_reporter::UsageLogReporter::new(client.clone(), session_token.clone());

        Self {
            client: client.clone(),
            auth_service: AuthService::with_client(client.clone()),
            version_client: VersionClient::with_client(client.clone()),
            session_token,
            usage_reporter: usage_reporter.clone(),
            operation_coordinator: {
                OperationCoordinator::new(
                    None,
                    Some(permission_gate),
                    Some(usage_reporter.clone()),
                    Some(operation_log_buffer.clone()),
                    None,
                )
            },
            device_runtime: commands::device::DeviceRuntime::new(),
            firmware_artifacts: commands::firmware::FirmwareArtifactRuntime::new(),
            firmware_extraction: commands::firmware::FirmwareExtractionRuntime::new(),
            payload_inspection: commands::firmware::PayloadInspectionRuntime::new(),
            firmware_progress: commands::firmware::FirmwareProgressRuntime::new(),
            prepared_firmware_artifact: commands::quick_flash::PreparedFirmwareArtifactRuntime::new(
            ),
            prepared_dual_slot: commands::quick_flash::PreparedDualSlotRuntime::new(),
            partition_workspace: commands::partitions::PartitionWorkspaceRuntime::new(),
            mirror_runtime: commands::mirror::MirrorRuntime::new(),
            root_image_runtime: commands::root::RootImageRuntime::new(),
            root_patched_artifacts: commands::root::RootPatchedArtifactRuntime::new(),
            root_ota_runtime: commands::root_ota::RootOtaRuntime::new(),
            safe_flash_runtime: commands::safe_flash::SafeFlashRuntime::new(),
            session_events_rx: Mutex::new(Some(session_events_rx)),
            operation_log_store,
            session_lifecycle: SessionLifecycle::new(
                heartbeat_fn,
                Some(on_force_exit),
                Some(on_update_required),
            ),
        }
    }

    pub fn bind_operation_events(&self, app_handle: AppHandle<Wry>) {
        let coordinator = self.operation_coordinator.clone();
        let device_runtime = self.device_runtime.clone();
        spawn(async move {
            let mut receiver = coordinator.subscribe_state();
            let mut was_device_busy = false;
            while let Ok(snapshot) = receiver.recv().await {
                let is_busy = matches!(
                    snapshot.kind,
                    OperationKind::Discovering
                        | OperationKind::Rebooting
                        | OperationKind::Installing
                        | OperationKind::Transferring
                        | OperationKind::Hashing
                        | OperationKind::Flashing
                        | OperationKind::Mirroring
                );

                let _ = app_handle.emit(
                    "operation:snapshot",
                    OperationSnapshotPayload {
                        kind: snapshot.kind,
                        operation_id: snapshot.operation_id,
                        title: snapshot.title,
                        stage: snapshot.stage,
                        progress: snapshot.progress,
                        started_at: snapshot.started_at,
                        is_cancellable: snapshot.is_cancellable,
                        partition_task: snapshot.partition_task,
                        partition_tasks: snapshot.partition_tasks,
                        is_busy,
                    },
                );

                if should_compensate_device_refresh(was_device_busy, is_busy) {
                    let app_handle = app_handle.clone();
                    let device_runtime = device_runtime.clone();
                    spawn(async move {
                        let update =
                            commands::device::automatic_device_refresh(&device_runtime, false)
                                .await;
                        if update.should_emit {
                            let _ = app_handle.emit("device:snapshot", update.snapshot);
                        }
                    });
                }
                was_device_busy = is_busy;
            }
        });
    }

    pub fn bind_firmware_progress_events(&self, app_handle: AppHandle<Wry>) {
        self.firmware_progress.bind_sink(move |progress| {
            let _ = app_handle.emit(commands::firmware::FIRMWARE_PROGRESS_EVENT, progress);
        });
    }

    pub fn bind_session_events(&self, app_handle: AppHandle<Wry>) {
        let mut receiver = {
            let mut guard = self
                .session_events_rx
                .lock()
                .expect("session event receiver lock should not be poisoned");
            guard.take()
        };

        let coordinator = self.operation_coordinator.clone();
        if let Some(mut receiver) = receiver.take() {
            spawn(async move {
                while let Some(event) = receiver.recv().await {
                    match event {
                        SessionLifecycleEvent::ForceExit(reason) => {
                            finalize_operation_before_exit(&coordinator).await;
                            let _ = app_handle
                                .emit(SESSION_FORCE_EXIT_EVENT, SessionForceExitPayload { reason });
                            app_handle.exit(0);
                        }
                        SessionLifecycleEvent::UpdateRequired(update) => {
                            finalize_operation_before_exit(&coordinator).await;
                            let _ = app_handle.emit(
                                SESSION_UPDATE_REQUIRED_EVENT,
                                SessionUpdateRequiredPayload::from(update),
                            );
                        }
                    }
                }
            });
        }
    }

    pub fn bind_device_monitor(&self, app_handle: AppHandle<Wry>) {
        let coordinator = self.operation_coordinator.clone();
        let runtime = self.device_runtime.clone();
        let mirror_runtime = self.mirror_runtime.clone();
        spawn(async move {
            loop {
                sleep(Duration::from_secs(3)).await;
                let update =
                    commands::device::automatic_device_refresh(&runtime, coordinator.is_busy())
                        .await;
                let _ = commands::mirror::reconcile_after_device_update(
                    &mirror_runtime,
                    &runtime,
                    &coordinator,
                )
                .await;
                if update.should_emit {
                    let _ = app_handle.emit("device:snapshot", update.snapshot);
                }
            }
        });
    }
}

async fn finalize_operation_before_exit(coordinator: &OperationCoordinator) {
    coordinator.cancel_current().await;

    let mut waited = Duration::from_millis(0);
    while coordinator.is_busy() && waited < SESSION_SHUTDOWN_WAIT {
        sleep(Duration::from_millis(50)).await;
        waited += Duration::from_millis(50);
    }
}

fn should_compensate_device_refresh(was_device_busy: bool, is_device_busy: bool) -> bool {
    was_device_busy && !is_device_busy
}

#[derive(Serialize, Clone)]
struct SessionForceExitPayload {
    reason: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SessionUpdateRequiredPayload {
    message: String,
    latest: Option<String>,
    min_version: Option<String>,
    download_url: Option<String>,
}

impl From<UpdateRequiredInfo> for SessionUpdateRequiredPayload {
    fn from(value: UpdateRequiredInfo) -> Self {
        Self {
            message: value.message,
            latest: value.latest,
            min_version: value.min_version,
            download_url: value.download_url,
        }
    }
}

#[derive(Serialize)]
pub struct VersionCheckResponse {
    pub latest: Option<String>,
    pub min_version: Option<String>,
    pub download_url: Option<String>,
    pub update_required: bool,
    pub force_update: bool,
}

impl From<VersionCheckResult> for VersionCheckResponse {
    fn from(value: VersionCheckResult) -> Self {
        Self {
            latest: value.latest,
            min_version: value.min_version,
            download_url: value.download_url,
            update_required: value.update_required,
            force_update: value.force_update,
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct OperationSnapshotPayload {
    kind: OperationKind,
    operation_id: Option<String>,
    title: String,
    stage: String,
    progress: Option<f64>,
    started_at: Option<i64>,
    is_cancellable: bool,
    partition_task: Option<nwflash_domain::PartitionTaskSnapshot>,
    partition_tasks: Vec<nwflash_domain::PartitionTaskSnapshot>,
    is_busy: bool,
}

#[derive(Debug)]
struct OperationLogBuffer {
    entries: Arc<OperationLogStore>,
}

impl OperationLogger for OperationLogBuffer {
    fn write(
        &self,
        level: nwflash_domain::OperationLogLevel,
        message: String,
        operation_id: Option<String>,
    ) {
        self.entries.write(level, message, operation_id);
    }
}

#[cfg(test)]
mod device_monitor_tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use nwflash_domain::OperationKind;

    use super::{should_compensate_device_refresh, AppState};

    #[test]
    fn operation_completion_requests_one_compensating_device_refresh() {
        assert!(should_compensate_device_refresh(true, false));
        assert!(!should_compensate_device_refresh(false, false));
        assert!(!should_compensate_device_refresh(true, true));
    }

    #[test]
    fn app_state_owns_the_firmware_artifact_runtime() {
        let state = super::AppState::new();
        assert!(state.firmware_artifacts.get("missing-artifact").is_err());
        assert!(state
            .payload_inspection
            .resolve_selected(&["0".to_string()])
            .is_err());
    }

    #[tokio::test]
    async fn app_state_rejects_flashing_without_cloudflare_operation_authorization() {
        let state = AppState::new();
        let operation_started = Arc::new(AtomicBool::new(false));
        let operation_started_for_run = operation_started.clone();

        let result = state
            .operation_coordinator
            .run_async(
                OperationKind::Flashing,
                "线刷测试",
                move |_, _| async move {
                    operation_started_for_run.store(true, Ordering::Release);
                    Ok(())
                },
            )
            .await;

        assert!(result.is_err());
        assert!(!operation_started.load(Ordering::Acquire));
    }
}

pub fn run_app(context: tauri::Context<Wry>) -> tauri::Result<()> {
    let app_builder: tauri::Builder<Wry> = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init());
    #[cfg(feature = "e2e")]
    let app_builder = app_builder.plugin(tauri_plugin_wdio::init());
    #[cfg(feature = "e2e")]
    let app_builder = app_builder.plugin(tauri_plugin_wdio_webdriver::init());
    let app_builder = app_builder
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("奶蛙Flash");
            }
            let state = app.state::<AppState>();
            state.bind_operation_events(app.handle().clone());
            state.bind_firmware_progress_events(app.handle().clone());
            state.bind_session_events(app.handle().clone());
            state.bind_device_monitor(app.handle().clone());
            Ok(())
        })
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::auth::auth_login,
            commands::auth::auth_logout,
            commands::auth::auth_validate_token,
            commands::firmware::firmware_inspect_local,
            commands::firmware::firmware_inspect_payload_local,
            commands::firmware::firmware_extract_payload_local,
            commands::firmware::firmware_inspect_line_flash_package,
            commands::firmware::firmware_extract_vivo_local,
            commands::firmware::firmware_prepare_line_flash_artifact,
            commands::firmware::firmware_prepare_extracted_artifact,
            commands::version::version_check,
            commands::quick_flash::quick_flash_inspect_image,
            commands::quick_flash::quick_flash_prepare_boot_image,
            commands::quick_flash::quick_flash_prepare_preset_image,
            commands::quick_flash::quick_flash_prepare_firmware_artifact,
            commands::quick_flash::quick_flash_prepare_dual_slot_preset_image,
            commands::quick_flash::quick_flash_execute_boot_image,
            commands::quick_flash::quick_flash_execute_preset_image,
            commands::quick_flash::quick_flash_execute_firmware_artifact,
            commands::quick_flash::quick_flash_execute_prepared_dual_slot_preset,
            commands::root::root_preflight,
            commands::root::root_select_image,
            commands::root::root_install_manager,
            commands::root::root_patch_vivo_ksu,
            commands::root::root_patch_official_vendor_boot,
            commands::root::root_prepare_patched_artifact_flash,
            commands::root::root_execute_patched_artifact_flash,
            commands::root::root_run_automatic,
            commands::root_ota::root_ota_check,
            commands::root_ota::root_ota_extract_images,
            commands::files::files_list,
            commands::files::files_delete,
            commands::files::files_download,
            commands::files::files_upload,
            commands::files::files_install_apk,
            commands::mirror::mirror_status,
            commands::mirror::mirror_start,
            commands::mirror::mirror_stop,
            commands::mirror::mirror_set_auto,
            commands::safe_flash::safe_flash_prepare_online,
            commands::safe_flash::safe_flash_prepare_local_source,
            commands::safe_flash::safe_flash_prepare_local_directory,
            commands::safe_flash::safe_flash_execute_prepared,
            commands::safe_flash::safe_flash_cancel_prepared,
            commands::session::session_start,
            commands::session::session_stop,
            commands::session::session_state,
            commands::operation_log::operation_logs_snapshot,
            commands::operation_log::operation_logs_clear,
            commands::operation::operation_cancel,
            commands::partitions::partitions_refresh,
            commands::partitions::partitions_prepare_erase,
            commands::partitions::partitions_execute_erase,
            commands::partitions::partitions_map_images,
            commands::partitions::partitions_prepare_write,
            commands::partitions::partitions_execute_write,
            commands::partitions::partitions_prepare_backup,
            commands::partitions::partitions_execute_backup,
            commands::online::online_sessions,
            commands::software::software_status,
            commands::drivers::driver_reinstall,
            commands::resources::resource_inventory,
            commands::resources::resource_install,
            commands::device::device_refresh,
            commands::device::device_reboot_system,
            commands::device::device_reboot_bootloader,
            commands::device::device_reboot_fastboot,
        ]);

    app_builder.run(context)
}
