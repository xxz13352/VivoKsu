use std::{
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use nwflash_application::{
    HeartbeatInput, OperationAuthorization, OperationCoordinator, OperationIdleLease,
    OperationLogger, OperationPermissionGate, SessionLifecycle,
};
use nwflash_domain::{DomainError, OperationKind};
use nwflash_infrastructure::{
    api_client::UpdateRequiredInfo, AuthService, CloudflareClient, CloudflareError,
    HeartbeatAdmission, OperationLogStore, ProcessIdentity, SecretToken, VersionCheckResult,
    VersionClient,
};
use serde::Serialize;
use tauri::{async_runtime::spawn, AppHandle, Emitter, Manager, Wry};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::time::sleep;

mod commands;
#[allow(dead_code)]
mod session_capabilities;

#[doc(hidden)]
pub use commands::mirror::{start_plan, MirrorRuntime};
mod usage_reporter;

pub const APP_LABEL: &str = "奶蛙Flash";
const SESSION_FORCE_EXIT_EVENT: &str = "session:force-exit";
const SESSION_UPDATE_REQUIRED_EVENT: &str = "session:update-required";

/// Mirrors `ServerOperationGate.AuthorizeTimeout` in the WPF build.  Server
/// authorization is advisory: a ban answers "denied", but an unreachable or slow
/// server must not block device work, and must never pin the single-permit
/// operation gate while a request hangs.
const AUTHORIZE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
enum SessionLifecycleEvent {
    ForceExit(String),
    UpdateRequired(UpdateRequiredInfo),
}

pub struct AppState {
    pub client: CloudflareClient,
    pub auth_service: AuthService,
    pub version_client: VersionClient,
    pub session_token: Arc<RwLock<Option<SecretToken>>>,
    pub process_identity: ProcessIdentity,
    pub usage_reporter: Arc<usage_reporter::UsageLogReporter>,
    pub session_lifecycle: SessionLifecycle,
    pub operation_coordinator: OperationCoordinator,
    pub device_runtime: commands::device::DeviceRuntime,
    pub firmware_artifacts: commands::firmware::FirmwareArtifactRuntime,
    pub firmware_extraction: commands::firmware::FirmwareExtractionRuntime,
    pub payload_inspection: commands::firmware::PayloadInspectionRuntime,
    pub remote_firmware_inspection: commands::firmware::RemoteFirmwareInspectionRuntime,
    pub(crate) firmware_progress: commands::firmware::FirmwareProgressRuntime,
    pub prepared_firmware_artifact: commands::quick_flash::PreparedFirmwareArtifactRuntime,
    pub prepared_dual_slot: commands::quick_flash::PreparedDualSlotRuntime,
    pub partition_workspace: commands::partitions::PartitionWorkspaceRuntime,
    pub mirror_runtime: commands::mirror::MirrorRuntime,
    pub(crate) session_capabilities: Arc<session_capabilities::SessionCapabilityScope>,
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
    session_token: Arc<RwLock<Option<SecretToken>>>,
}

impl CloudflareOperationPermissionGate {
    fn new(client: CloudflareClient, session_token: Arc<RwLock<Option<SecretToken>>>) -> Self {
        Self {
            client,
            session_token,
        }
    }
}

/// Maps an authorization request failure onto a permission decision.
///
/// Server authorization is advisory (`ServerOperationGate` in the WPF build):
/// an explicit 401 (session revoked), 426 (client too old), or local API
/// integrity failure blocks the user. Network faults and 5xx remain advisory,
/// because an unreachable server must not stop someone from flashing a phone
/// that is already in hand; a banned account is still force-exited by the
/// heartbeat within seconds.
fn authorization_for_error(error: &CloudflareError) -> OperationAuthorization {
    if matches!(error, CloudflareError::Integrity(_)) {
        return OperationAuthorization::deny("网络完整性校验失败，已拒绝本次操作。");
    }
    match error.status_code() {
        Some(401) => OperationAuthorization::deny("登录已失效，请联系管理员。"),
        Some(426) => OperationAuthorization::deny(format!("需要更新 {APP_LABEL} 后才能继续使用。")),
        _ => OperationAuthorization::allow(),
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
                .as_ref()
                .filter(|token| !token.is_empty())
                .map(SecretToken::request_scope)
                .ok_or_else(|| {
                    DomainError::AuthorizationDenied("未登录，无法执行受控操作。".to_string())
                })?;

            let operation_label = format!("{operation:?}");
            let request = client.authorize_operation(token.as_str(), &operation_label, &title);
            match tokio::time::timeout(AUTHORIZE_TIMEOUT, request).await {
                Ok(Ok(authorization)) => Ok(OperationAuthorization {
                    allowed: authorization.allowed,
                    reason: authorization.reason,
                }),
                // Only an explicit "this account may not do this" answer blocks the
                // user; everything else defaults to allow.
                Ok(Err(error)) => Ok(authorization_for_error(&error)),
                // Black-holed request: never hold the global operation gate for it.
                Err(_) => Ok(OperationAuthorization::allow()),
            }
        })
    }
}

impl AppState {
    pub fn try_new() -> Result<Self, CloudflareError> {
        let client = CloudflareClient::new_default()?;
        Self::try_with_client(client)
    }

    #[cfg(not(test))]
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|_| panic!("pinned API client initialization failed closed"))
    }

    #[cfg(test)]
    pub fn new() -> Self {
        Self::try_with_client(CloudflareClient::new_injected(
            nwflash_infrastructure::DEFAULT_BASE_URL,
            nwflash_infrastructure::DEFAULT_APP_VERSION,
        ))
        .expect("debug AppState identity should initialize")
    }

    fn try_with_client(client: CloudflareClient) -> Result<Self, CloudflareError> {
        let (session_events_tx, session_events_rx) = unbounded_channel();
        let process_identity = ProcessIdentity::generate().map_err(CloudflareError::Integrity)?;
        let session_token = Arc::new(RwLock::new(None));
        let session_capabilities = Arc::new(session_capabilities::SessionCapabilityScope::new());
        let permission_gate = Arc::new(CloudflareOperationPermissionGate::new(
            client.clone(),
            session_token.clone(),
        ));

        let heartbeat_fn = {
            let heartbeat_auth = AuthService::with_client(client.clone());
            let heartbeat_identity = process_identity.clone();
            let heartbeat_capabilities = session_capabilities.clone();
            std::sync::Arc::new(move |input: HeartbeatInput| {
                let heartbeat_auth = heartbeat_auth.clone();
                let heartbeat_identity = heartbeat_identity.clone();
                let heartbeat_capabilities = heartbeat_capabilities.clone();
                let future: futures::future::BoxFuture<
                    'static,
                    Result<HeartbeatAdmission, CloudflareError>,
                > = Box::pin(async move {
                    let capability = if input.active {
                        Some(heartbeat_capabilities.capture().map_err(|_| {
                            CloudflareError::Integrity(
                                nwflash_infrastructure::IntegrityFailure::LeaseBinding,
                            )
                        })?)
                    } else {
                        None
                    };
                    let previous_sequence = input.lease.sequence();
                    let admission = heartbeat_auth
                        .heartbeat(
                            &input.token,
                            &input.username,
                            &heartbeat_identity,
                            &input.lease,
                            input.active,
                        )
                        .await?;
                    if let (Some(capability), HeartbeatAdmission::Accepted(next)) =
                        (capability, &admission)
                    {
                        heartbeat_capabilities
                            .refresh_verified(capability, previous_sequence, next.clone())
                            .map_err(|_| {
                                CloudflareError::Integrity(
                                    nwflash_infrastructure::IntegrityFailure::LeaseSequence,
                                )
                            })?;
                    }
                    Ok(admission)
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
        operation_log_store.start_new_session();
        let operation_log_buffer = Arc::new(OperationLogBuffer {
            entries: operation_log_store.clone(),
        });
        let usage_reporter =
            usage_reporter::UsageLogReporter::new(client.clone(), session_token.clone());
        Ok(Self {
            client: client.clone(),
            auth_service: AuthService::with_client(client.clone()),
            version_client: VersionClient::with_client(client.clone()),
            session_token,
            process_identity,
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
            remote_firmware_inspection: commands::firmware::RemoteFirmwareInspectionRuntime::new(),
            firmware_progress: commands::firmware::FirmwareProgressRuntime::new(),
            prepared_firmware_artifact:
                commands::quick_flash::PreparedFirmwareArtifactRuntime::with_scope(
                    session_capabilities.clone(),
                ),
            prepared_dual_slot: commands::quick_flash::PreparedDualSlotRuntime::with_scope(
                session_capabilities.clone(),
            ),
            partition_workspace: commands::partitions::PartitionWorkspaceRuntime::new(),
            mirror_runtime: commands::mirror::MirrorRuntime::new(),
            session_capabilities: session_capabilities.clone(),
            root_image_runtime: commands::root::RootImageRuntime::with_scope(
                session_capabilities.clone(),
            ),
            root_patched_artifacts: commands::root::RootPatchedArtifactRuntime::with_scope(
                session_capabilities.clone(),
            ),
            root_ota_runtime: commands::root_ota::RootOtaRuntime::with_scope(
                session_capabilities.clone(),
            ),
            safe_flash_runtime: commands::safe_flash::SafeFlashRuntime::with_scope(
                session_capabilities,
            ),
            session_events_rx: Mutex::new(Some(session_events_rx)),
            operation_log_store,
            session_lifecycle: SessionLifecycle::new(
                heartbeat_fn,
                Some(on_force_exit),
                Some(on_update_required),
            ),
        })
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

        if let Some(mut receiver) = receiver.take() {
            spawn(async move {
                while let Some(event) = receiver.recv().await {
                    match event {
                        SessionLifecycleEvent::ForceExit(reason) => {
                            let _ = app_handle
                                .emit(SESSION_FORCE_EXIT_EVENT, SessionForceExitPayload { reason });
                        }
                        SessionLifecycleEvent::UpdateRequired(update) => {
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

    pub(crate) fn revoke_root_capabilities(&self, _idle_lease: &OperationIdleLease) {
        let owned_roots = self.session_capabilities.invalidate(|| {
            let mut owned_roots = self.root_image_runtime.clear_owned();
            owned_roots.extend(self.root_patched_artifacts.clear_owned());
            owned_roots.extend(self.root_ota_runtime.clear_owned());
            owned_roots.extend(self.safe_flash_runtime.clear_owned());
            owned_roots.extend(self.firmware_artifacts.clear_owned());
            self.prepared_firmware_artifact.clear();
            self.prepared_dual_slot.clear();
            owned_roots
        });

        for owned_root in owned_roots {
            let _ = std::fs::remove_dir_all(owned_root);
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
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

fn normalize_operation_log_message(
    level: nwflash_domain::OperationLogLevel,
    message: String,
) -> Option<String> {
    let message = message.trim().to_string();
    if message.is_empty() || message.starts_with("准备 VIVO 线刷") {
        return None;
    }

    if level == nwflash_domain::OperationLogLevel::Info
        && matches!(
            message.as_str(),
            "连接服务器"
                | "正在连接服务器"
                | "连接服务端"
                | "正在连接服务端"
                | "请求服务"
                | "正在请求服务"
                | "请求服务器"
                | "正在请求服务器"
                | "检测服务器"
                | "正在检测服务器"
                | "检测服务器 OTA"
                | "检测服务器 OTA完成。"
                | "检测服务器 OTA已取消。"
                | "正在解析服务器 OTA"
                | "正在获取在线 OTA 信息"
                | "正在请求 OTA 服务器"
                | "正在请求 OTA 服务端"
        )
    {
        return None;
    }

    Some(match message.as_str() {
        "正在解析服务器 OTA"
        | "正在获取在线 OTA 信息"
        | "正在请求 OTA 服务器"
        | "正在请求 OTA 服务端" => "正在请求服务器".to_string(),
        "检测服务器 OTA" => "请求服务器".to_string(),
        "正在下载在线 OTA" => "正在下载在线固件".to_string(),
        "提取服务器 OTA 分区" => "提取服务器固件分区".to_string(),
        "正在探测 OTA 格式" => "正在探测固件格式".to_string(),
        _ => message.replace("OTA", "固件"),
    })
}

impl OperationLogger for OperationLogBuffer {
    fn write(
        &self,
        level: nwflash_domain::OperationLogLevel,
        message: String,
        operation_id: Option<String>,
    ) {
        if let Some(message) = normalize_operation_log_message(level, message) {
            self.entries.write(level, message, operation_id);
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod device_monitor_tests {
    use std::{
        fs,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        authorization_for_error, should_compensate_device_refresh, AppState, OperationLogBuffer,
    };
    use futures::future::BoxFuture;
    use nwflash_application::{
        OperationAuthorization, OperationCoordinator, OperationLogger, OperationPermissionGate,
    };
    use nwflash_domain::{DomainError, OperationKind, OperationLogLevel};
    use nwflash_infrastructure::{CloudflareError, IntegrityFailure};

    struct DenyAllOperations;

    impl OperationPermissionGate for DenyAllOperations {
        fn authorize(
            &self,
            _operation: OperationKind,
            _title: String,
        ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
            Box::pin(async { Ok(OperationAuthorization::deny("测试授权拒绝")) })
        }
    }

    #[test]
    fn operation_completion_requests_one_compensating_device_refresh() {
        assert!(should_compensate_device_refresh(true, false));
        assert!(!should_compensate_device_refresh(false, false));
        assert!(!should_compensate_device_refresh(true, true));
    }

    #[test]
    fn api_integrity_failure_never_falls_through_the_advisory_allow_path() {
        let authorization =
            authorization_for_error(&CloudflareError::Integrity(IntegrityFailure::SpkiMismatch));

        assert!(!authorization.allowed);
        assert!(authorization
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("完整性")));
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

    #[test]
    fn app_state_revocation_revokes_firmware_artifacts_and_removes_only_owned_staging() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let external_root = std::env::temp_dir().join(format!("nwflash-external-artifact-{nonce}"));
        let owned_root = std::env::temp_dir().join(format!("nwflash-owned-artifact-{nonce}"));
        fs::create_dir_all(&external_root).expect("external fixture root should be created");
        fs::create_dir_all(&owned_root).expect("owned staging root should be created");
        fs::write(external_root.join("external.img"), [1, 2, 3])
            .expect("external fixture image should be written");
        fs::write(owned_root.join("boot.img"), [1, 2, 3])
            .expect("owned staging image should be written");

        let state = AppState::new();
        state.session_capabilities.activate();
        state.firmware_artifacts.replace(
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: external_root
                    .join("external.img")
                    .to_string_lossy()
                    .into_owned(),
                size_bytes: 3,
            },
            external_root.clone(),
        );
        let artifact_id = crate::commands::firmware::register_owned_firmware_artifact_for_test(
            &state.firmware_artifacts,
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: owned_root.join("boot.img").to_string_lossy().into_owned(),
                size_bytes: 3,
            },
            owned_root.clone(),
        );
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should permit session revocation");

        state.revoke_root_capabilities(&idle_lease);

        let artifact_revoked = state.firmware_artifacts.get(&artifact_id).is_err();
        let owned_root_removed = !owned_root.exists();
        let external_root_preserved = external_root.is_dir();
        let _ = fs::remove_dir_all(&owned_root);
        let _ = fs::remove_dir_all(&external_root);

        assert!(artifact_revoked);
        assert!(owned_root_removed);
        assert!(external_root_preserved);
    }

    #[test]
    fn app_state_revocation_revokes_a_current_external_firmware_artifact_without_deleting_it() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let external_root =
            std::env::temp_dir().join(format!("nwflash-current-external-artifact-{nonce}"));
        let image_path = external_root.join("boot.img");
        fs::create_dir_all(&external_root).expect("external fixture root should be created");
        fs::write(&image_path, [1, 2, 3]).expect("external fixture image should be written");

        let state = AppState::new();
        state.session_capabilities.activate();
        let artifact_id = state.firmware_artifacts.replace(
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: image_path.to_string_lossy().into_owned(),
                size_bytes: 3,
            },
            external_root.clone(),
        );
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should permit session revocation");

        state.revoke_root_capabilities(&idle_lease);

        let artifact_revoked = state.firmware_artifacts.get(&artifact_id).is_err();
        let external_root_preserved = external_root.is_dir();
        let _ = fs::remove_dir_all(&external_root);

        assert!(artifact_revoked);
        assert!(external_root_preserved);
    }

    #[test]
    fn operation_log_omits_routine_server_probe_messages() {
        let entries = Arc::new(nwflash_infrastructure::OperationLogStore::new(None, 10));
        let buffer = OperationLogBuffer {
            entries: entries.clone(),
        };

        for message in [
            "连接服务器",
            "正在连接服务器",
            "请求服务",
            "正在请求服务器",
            "检测服务器 OTA",
            "正在解析服务器 OTA",
        ] {
            buffer.write(OperationLogLevel::Info, message.to_string(), None);
        }
        buffer.write(
            OperationLogLevel::Info,
            "正在下载在线 OTA".to_string(),
            None,
        );

        let messages = entries
            .snapshot()
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>();
        assert_eq!(messages, ["正在下载在线固件"]);
        assert!(messages.iter().all(|message| !message.contains("OTA")));
    }

    #[test]
    fn operation_log_omits_empty_messages_and_vivo_flash_prepare_titles() {
        let entries = Arc::new(nwflash_infrastructure::OperationLogStore::new(None, 10));
        let buffer = OperationLogBuffer {
            entries: entries.clone(),
        };

        for message in [
            "",
            "   ",
            "准备 VIVO 线刷",
            "准备 VIVO 线刷完成。",
            "准备 VIVO 线刷已取消。",
        ] {
            buffer.write(OperationLogLevel::Info, message.to_string(), None);
        }

        assert!(entries.snapshot().is_empty());
    }

    #[tokio::test]
    async fn operation_coordinator_rejects_flashing_without_authorization() {
        let operation_started = Arc::new(AtomicBool::new(false));
        let operation_started_for_run = operation_started.clone();
        let coordinator =
            OperationCoordinator::new(None, Some(Arc::new(DenyAllOperations)), None, None, None);

        let result = coordinator
            .run_async(
                OperationKind::Flashing,
                "授权门禁测试",
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
    let app_state = AppState::try_new().map_err(|error| {
        tauri::Error::Setup((Box::new(error) as Box<dyn std::error::Error>).into())
    })?;
    let app_builder: tauri::Builder<Wry> =
        tauri::Builder::default().plugin(tauri_plugin_dialog::init());
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
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::auth::auth_login,
            commands::auth::auth_logout,
            commands::auth::auth_validate_token,
            commands::firmware::firmware_inspect_local,
            commands::firmware::firmware_inspect_remote,
            commands::firmware::firmware_inspect_payload_local,
            commands::firmware::firmware_extract_payload_local,
            commands::firmware::firmware_inspect_line_flash_package,
            commands::firmware::firmware_extract_vivo_local,
            commands::firmware::firmware_extract_remote,
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
            commands::quick_flash::quick_flash_execute_preset_images,
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
            commands::partitions::partitions_cached_snapshot,
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
