use std::sync::{Arc, Mutex};

use nwflash_application::{
    parse_adb_battery_level, parse_adb_device_details, result_to_domain_error, DeviceMonitor,
    DeviceSession, MonitorRefreshResult, OperationAdmissionState, OperationCoordinator,
};
use nwflash_domain::{
    DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot, DomainError, OperationKind,
    OperationLogLevel,
};
use nwflash_infrastructure::OperationLogStore;
use nwflash_windows::{
    process::{run_command, run_command_with_cancel},
    DeviceTransport, PlatformDeviceDiscovery, PlatformTools, ProcessCommand, ProcessExecutor,
    ProcessOutput, SystemProcessExecutor,
};
use tauri::{AppHandle, Emitter, State};
use tokio::task;

use crate::AppState;

const DEVICE_DISCOVERY_PUBLIC_ERROR: &str = "设备检测失败，请检查设备连接后重试。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceDiscoveryFailure {
    Admission(&'static str),
    Scheduling,
    Discovery,
}

impl DeviceDiscoveryFailure {
    fn from_domain_error(error: DomainError) -> Self {
        match admission_reason_from_domain_error(&error) {
            Some(reason) => Self::Admission(reason),
            None => Self::Discovery,
        }
    }

    fn admission_reason(self) -> Option<&'static str> {
        match self {
            Self::Admission(reason) => Some(reason),
            Self::Scheduling | Self::Discovery => None,
        }
    }

    fn public_message(self) -> &'static str {
        match self {
            Self::Admission(reason) => match reason {
                "skipped:exit_pending" => "设备刷新已跳过（skipped:exit_pending）。",
                "skipped:terminating" => "设备刷新已跳过（skipped:terminating）。",
                "denied:flashing" => "设备刷新已跳过（denied:flashing）。",
                _ => "设备刷新已跳过。",
            },
            Self::Scheduling | Self::Discovery => DEVICE_DISCOVERY_PUBLIC_ERROR,
        }
    }

    fn log_reason(self) -> &'static str {
        match self {
            Self::Admission(reason) => reason,
            Self::Scheduling => "failed:scheduling",
            Self::Discovery => "failed:discovery",
        }
    }
}

#[derive(Clone)]
pub(crate) struct AdmissionCheckedExecutor<E, H = fn()> {
    coordinator: OperationCoordinator,
    operation: OperationKind,
    inner: E,
    before_final_check: H,
}

impl<E> AdmissionCheckedExecutor<E> {
    pub(crate) fn new(
        coordinator: OperationCoordinator,
        operation: OperationKind,
        inner: E,
    ) -> Self {
        Self {
            coordinator,
            operation,
            inner,
            before_final_check: || {},
        }
    }
}

#[cfg(test)]
impl<E, H> AdmissionCheckedExecutor<E, H> {
    pub(crate) fn with_hook(
        coordinator: OperationCoordinator,
        inner: E,
        before_final_check: H,
    ) -> Self {
        Self {
            coordinator,
            operation: OperationKind::Idle,
            inner,
            before_final_check,
        }
    }
}

impl<E, H> ProcessExecutor for AdmissionCheckedExecutor<E, H>
where
    E: ProcessExecutor,
    H: Fn() + Send + Sync,
{
    fn run(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
        (self.before_final_check)();
        if let Some(reason) =
            device_refresh_block_reason(self.coordinator.admission_state(), self.operation)
        {
            return Err(DomainError::AuthorizationDenied(admission_denial_message(
                reason,
            )));
        }
        self.inner.run(command)
    }
}

fn admission_denial_message(reason: &'static str) -> String {
    format!("设备检测已跳过（{reason}）。")
}

pub(crate) fn admission_reason_from_domain_error(error: &DomainError) -> Option<&'static str> {
    let DomainError::AuthorizationDenied(message) = error else {
        return None;
    };
    [
        "skipped:exit_pending",
        "skipped:terminating",
        "denied:flashing",
    ]
    .into_iter()
    .find(|reason| message == &admission_denial_message(reason))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSnapshotUpdate {
    pub snapshot: DeviceSnapshot,
    pub should_emit: bool,
}

#[derive(Clone)]
pub struct DeviceRuntime {
    monitor: Arc<Mutex<DeviceMonitor>>,
}

impl DeviceRuntime {
    pub fn new() -> Self {
        Self {
            monitor: Arc::new(Mutex::new(DeviceMonitor::new(
                DeviceSnapshot::disconnected(),
            ))),
        }
    }

    pub fn apply_snapshot(
        &self,
        snapshot: DeviceSnapshot,
        is_device_busy: bool,
        mode: DeviceRefreshMode,
    ) -> DeviceSnapshotUpdate {
        let mut monitor = self
            .monitor
            .lock()
            .expect("device monitor lock should not be poisoned");
        let result = monitor.refresh(snapshot, is_device_busy, mode);
        DeviceSnapshotUpdate {
            snapshot: monitor.snapshot().clone(),
            should_emit: matches!(result, MonitorRefreshResult::AppliedAndBroadcast),
        }
    }

    pub fn active_adb_serial(&self) -> Result<String, String> {
        let monitor = self
            .monitor
            .lock()
            .expect("device monitor lock should not be poisoned");
        let snapshot = monitor.snapshot();
        if snapshot.connection_state != DeviceConnectionState::AdbConnected
            || snapshot.serial.trim().is_empty()
            || snapshot.serial == "--"
        {
            return Err("当前没有可重启的 ADB 设备。".to_string());
        }

        Ok(snapshot.serial.clone())
    }

    fn snapshot(&self) -> DeviceSnapshot {
        self.monitor
            .lock()
            .expect("device monitor lock should not be poisoned")
            .snapshot()
            .clone()
    }

    pub fn active_fastboot_serial(&self) -> Result<String, String> {
        let monitor = self
            .monitor
            .lock()
            .expect("device monitor lock should not be poisoned");
        let snapshot = monitor.snapshot();
        if snapshot.connection_state != DeviceConnectionState::FastbootConnected
            || snapshot.serial.trim().is_empty()
            || snapshot.serial == "--"
        {
            return Err("当前没有可刷写的 Fastboot 设备。".to_string());
        }

        Ok(snapshot.serial.clone())
    }

    pub fn active_reboot_device(&self) -> Result<(DeviceConnectionState, String), String> {
        let monitor = self
            .monitor
            .lock()
            .expect("device monitor lock should not be poisoned");
        let snapshot = monitor.snapshot();
        if !matches!(
            snapshot.connection_state,
            DeviceConnectionState::AdbConnected | DeviceConnectionState::FastbootConnected
        ) || snapshot.serial.trim().is_empty()
            || snapshot.serial == "--"
        {
            return Err("当前没有可重启的 ADB 或 Fastboot 设备。".to_string());
        }

        Ok((snapshot.connection_state, snapshot.serial.clone()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceRebootTarget {
    System,
    Bootloader,
    Fastboot,
}

pub fn build_reboot_command(
    serial: &str,
    target: DeviceRebootTarget,
) -> Result<ProcessCommand, String> {
    let transport = DeviceTransport::new(PlatformTools::bundled());
    match target {
        DeviceRebootTarget::System => transport.build_adb_reboot_system_command(serial),
        DeviceRebootTarget::Bootloader => transport.build_adb_reboot_bootloader_command(serial),
        DeviceRebootTarget::Fastboot => transport.build_adb_reboot_fastboot_command(serial),
    }
    .map_err(|error| error.to_string())
}

pub fn build_reboot_command_for_connection(
    serial: &str,
    connection_state: DeviceConnectionState,
    target: DeviceRebootTarget,
) -> Result<ProcessCommand, String> {
    let transport = DeviceTransport::new(PlatformTools::bundled());
    let command = match connection_state {
        DeviceConnectionState::AdbConnected => build_reboot_command(serial, target),
        DeviceConnectionState::FastbootConnected => {
            let fastboot_target = match target {
                DeviceRebootTarget::System => None,
                DeviceRebootTarget::Bootloader => Some("bootloader"),
                DeviceRebootTarget::Fastboot => Some("fastboot"),
            };
            transport
                .build_fastboot_reboot_target_command(serial, fastboot_target)
                .map_err(|error| error.to_string())
        }
        _ => return Err("当前设备连接状态不支持重启。".to_string()),
    };

    command.map_err(|error| error.to_string())
}

#[cfg(test)]
pub async fn automatic_device_refresh(
    runtime: &DeviceRuntime,
    is_device_busy: bool,
) -> DeviceSnapshotUpdate {
    let operation = if is_device_busy {
        OperationKind::Flashing
    } else {
        OperationKind::Idle
    };
    automatic_device_refresh_with_admission(runtime, OperationAdmissionState::Running, operation)
        .await
}

/// Automatic refresh entry point for callers that can provide the coordinator
/// admission snapshot. The legacy bool-only wrapper above maps busy to
/// `Flashing` until the monitor binding is migrated to this function.
#[cfg(test)]
pub async fn automatic_device_refresh_with_admission(
    runtime: &DeviceRuntime,
    admission: OperationAdmissionState,
    operation: OperationKind,
) -> DeviceSnapshotUpdate {
    if device_refresh_is_blocked(admission, operation) {
        return DeviceSnapshotUpdate {
            snapshot: runtime.snapshot(),
            should_emit: false,
        };
    }

    let snapshot = discover_current_device()
        .await
        .unwrap_or_else(|_| discovery_error_snapshot());
    runtime.apply_snapshot(snapshot, false, DeviceRefreshMode::Automatic)
}

pub async fn automatic_device_refresh_guarded_with_log(
    runtime: &DeviceRuntime,
    coordinator: &OperationCoordinator,
    operation_log_store: Option<&OperationLogStore>,
) -> DeviceSnapshotUpdate {
    let Ok(_idle) = coordinator.try_acquire_idle() else {
        let operation = coordinator.state().await.kind;
        let reason = device_refresh_block_reason(coordinator.admission_state(), operation)
            .unwrap_or("denied:busy");
        if let Some(log) = operation_log_store {
            record_refresh_gate(log, "设备自动刷新", reason);
        }
        return DeviceSnapshotUpdate {
            snapshot: runtime.snapshot(),
            should_emit: false,
        };
    };
    let result = discover_current_device_guarded_with(
        coordinator,
        operation_log_store,
        "设备自动刷新",
        || {},
        {
            let coordinator = coordinator.clone();
            move || discover_current_device_blocking_guarded(coordinator)
        },
    )
    .await;
    project_automatic_discovery_result(runtime, result)
}

fn project_automatic_discovery_result(
    runtime: &DeviceRuntime,
    result: Result<DeviceSnapshot, DeviceDiscoveryFailure>,
) -> DeviceSnapshotUpdate {
    match result {
        Ok(snapshot) => runtime.apply_snapshot(snapshot, false, DeviceRefreshMode::Automatic),
        Err(error) if error.admission_reason().is_some() => DeviceSnapshotUpdate {
            snapshot: runtime.snapshot(),
            should_emit: false,
        },
        Err(_) => runtime.apply_snapshot(
            discovery_error_snapshot(),
            false,
            DeviceRefreshMode::Automatic,
        ),
    }
}

#[cfg(test)]
pub(crate) fn device_refresh_is_blocked(
    admission: OperationAdmissionState,
    operation: OperationKind,
) -> bool {
    device_refresh_block_reason(admission, operation).is_some()
}

pub(crate) fn device_refresh_block_reason(
    admission: OperationAdmissionState,
    operation: OperationKind,
) -> Option<&'static str> {
    match admission {
        OperationAdmissionState::ExitPending => Some("skipped:exit_pending"),
        OperationAdmissionState::Terminating => Some("skipped:terminating"),
        OperationAdmissionState::Running if operation == OperationKind::Flashing => {
            Some("denied:flashing")
        }
        OperationAdmissionState::Running => None,
    }
}

pub(crate) fn record_refresh_gate(
    operation_log_store: &OperationLogStore,
    operation: &'static str,
    reason: &'static str,
) {
    operation_log_store.write(
        OperationLogLevel::Warning,
        format!("{operation}已跳过（{reason}）。"),
        None,
    );
}

fn record_refresh_failure(
    operation_log_store: &OperationLogStore,
    operation: &'static str,
    reason: &'static str,
) {
    operation_log_store.write(
        OperationLogLevel::Warning,
        format!("{operation}失败（{reason}）。"),
        None,
    );
}

async fn discover_current_device_guarded_with<BeforeSpawn, Discover>(
    coordinator: &OperationCoordinator,
    operation_log_store: Option<&OperationLogStore>,
    operation: &'static str,
    before_spawn: BeforeSpawn,
    discover: Discover,
) -> Result<DeviceSnapshot, DeviceDiscoveryFailure>
where
    BeforeSpawn: FnOnce() + Send + 'static,
    Discover: FnOnce() -> Result<DeviceSnapshot, DomainError> + Send + 'static,
{
    let coordinator = coordinator.clone();
    let result = match task::spawn_blocking(move || {
        before_spawn();
        if let Some(reason) =
            device_refresh_block_reason(coordinator.admission_state(), OperationKind::Idle)
        {
            return Err(DeviceDiscoveryFailure::Admission(reason));
        }
        discover().map_err(DeviceDiscoveryFailure::from_domain_error)
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(DeviceDiscoveryFailure::Scheduling),
    };

    if let (Err(error), Some(log)) = (result.as_ref(), operation_log_store) {
        if let Some(reason) = error.admission_reason() {
            record_refresh_gate(log, operation, reason);
        } else {
            record_refresh_failure(log, operation, error.log_reason());
        }
    }
    result
}

pub(crate) async fn discover_current_device() -> Result<DeviceSnapshot, String> {
    task::spawn_blocking(discover_current_device_blocking)
        .await
        .map_err(|_| DEVICE_DISCOVERY_PUBLIC_ERROR.to_string())?
        .map_err(|_| DEVICE_DISCOVERY_PUBLIC_ERROR.to_string())
}

fn discover_current_device_blocking() -> Result<DeviceSnapshot, DomainError> {
    let tools = PlatformTools::bundled();
    let discovery = PlatformDeviceDiscovery::new(tools.clone());
    let snapshot = DeviceSession::refresh(&discovery)?;
    if snapshot.connection_state != DeviceConnectionState::AdbConnected {
        return Ok(snapshot);
    }

    let transport = DeviceTransport::new(tools);
    let properties = readonly_stdout(transport.build_adb_getprop_command(&snapshot.serial));
    let battery = readonly_stdout(transport.build_adb_battery_command(&snapshot.serial));
    Ok(enrich_adb_snapshot(snapshot, &properties, &battery))
}

fn discover_current_device_blocking_guarded(
    coordinator: OperationCoordinator,
) -> Result<DeviceSnapshot, DomainError> {
    let tools = PlatformTools::bundled();
    let executor =
        AdmissionCheckedExecutor::new(coordinator, OperationKind::Idle, SystemProcessExecutor);
    let discovery = PlatformDeviceDiscovery::with_executor(tools.clone(), executor.clone());
    let snapshot = DeviceSession::refresh(&discovery)?;
    if snapshot.connection_state != DeviceConnectionState::AdbConnected {
        return Ok(snapshot);
    }

    let transport = DeviceTransport::new(tools);
    let properties = readonly_stdout_with_executor(
        transport.build_adb_getprop_command(&snapshot.serial),
        &executor,
    )?;
    let battery = readonly_stdout_with_executor(
        transport.build_adb_battery_command(&snapshot.serial),
        &executor,
    )?;
    Ok(enrich_adb_snapshot(snapshot, &properties, &battery))
}

fn readonly_stdout(command: Result<ProcessCommand, DomainError>) -> String {
    match command.and_then(run_command) {
        Ok(output) if output.exit_code == 0 => output.stdout,
        _ => String::new(),
    }
}

fn readonly_stdout_with_executor<E: ProcessExecutor>(
    command: Result<ProcessCommand, DomainError>,
    executor: &E,
) -> Result<String, DomainError> {
    match command.and_then(|command| executor.run(command)) {
        Ok(output) if output.exit_code == 0 => Ok(output.stdout),
        Err(error) if admission_reason_from_domain_error(&error).is_some() => Err(error),
        Ok(_) | Err(_) => Ok(String::new()),
    }
}

fn enrich_adb_snapshot(
    mut snapshot: DeviceSnapshot,
    properties: &str,
    battery: &str,
) -> DeviceSnapshot {
    let details = parse_adb_device_details(&snapshot.serial, properties);
    snapshot.model = details.model;
    snapshot.android_version = details.android_version;
    snapshot.battery_level = parse_adb_battery_level(battery);
    snapshot
}

fn discovery_error_snapshot() -> DeviceSnapshot {
    DeviceSnapshot {
        connection_state: DeviceConnectionState::Error,
        serial: "--".to_string(),
        connection_label: "设备检测失败".to_string(),
        model: "未检测到设备".to_string(),
        android_version: "--".to_string(),
        battery_level: "--".to_string(),
    }
}

#[tauri::command]
pub async fn device_refresh(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Result<DeviceSnapshot, String> {
    let operation = state.operation_coordinator.state().await.kind;
    let admission = state.operation_coordinator.admission_state();
    if let Some(reason) = device_refresh_block_reason(admission, operation) {
        record_refresh_gate(&state.operation_log_store, "设备刷新", reason);
        return Err(format!("设备刷新已跳过（{reason}）。"));
    }
    let _idle = match state.operation_coordinator.try_acquire_idle() {
        Ok(lease) => lease,
        Err(error) => {
            record_refresh_gate(&state.operation_log_store, "设备刷新", "denied:busy");
            return Err(error.to_string());
        }
    };
    let snapshot = discover_current_device_guarded_with(
        &state.operation_coordinator,
        Some(&state.operation_log_store),
        "设备刷新",
        || {},
        {
            let coordinator = state.operation_coordinator.clone();
            move || discover_current_device_blocking_guarded(coordinator)
        },
    )
    .await
    .map_err(|error| error.public_message().to_string())?;

    let update = state.device_runtime.apply_snapshot(
        snapshot,
        state.operation_coordinator.is_busy(),
        DeviceRefreshMode::Manual,
    );
    let _ = crate::commands::mirror::reconcile_after_device_update(
        &state.mirror_runtime,
        &state.device_runtime,
        &state.operation_coordinator,
    )
    .await;
    if update.should_emit {
        app_handle
            .emit("device:snapshot", update.snapshot.clone())
            .map_err(|error| format!("设备状态事件发送失败：{error}"))?;
    }

    Ok(update.snapshot)
}

#[tauri::command]
pub async fn device_reboot_system(state: State<'_, AppState>) -> Result<(), String> {
    device_reboot(&state, DeviceRebootTarget::System).await
}

#[tauri::command]
pub async fn device_reboot_bootloader(state: State<'_, AppState>) -> Result<(), String> {
    device_reboot(&state, DeviceRebootTarget::Bootloader).await
}

#[tauri::command]
pub async fn device_reboot_fastboot(state: State<'_, AppState>) -> Result<(), String> {
    device_reboot(&state, DeviceRebootTarget::Fastboot).await
}

async fn device_reboot(state: &AppState, target: DeviceRebootTarget) -> Result<(), String> {
    let (connection_state, serial) = state.device_runtime.active_reboot_device()?;
    let command = build_reboot_command_for_connection(&serial, connection_state, target)?;
    let title = match target {
        DeviceRebootTarget::System => "重启到系统",
        DeviceRebootTarget::Bootloader => "重启到 Bootloader",
        DeviceRebootTarget::Fastboot => "重启到 Fastbootd",
    };

    state
        .operation_coordinator
        .run_async(
            OperationKind::Rebooting,
            title,
            move |context, cancellation| async move {
                context.report_stage(title);
                let cancellation_for_command = cancellation.clone();
                let output = task::spawn_blocking(move || {
                    run_command_with_cancel(command, None, move || {
                        cancellation_for_command.is_cancelled()
                    })
                })
                .await
                .map_err(|error| DomainError::Internal(format!("重启命令调度失败：{error}")))??;

                if output.exit_code != 0 {
                    return Err(DomainError::ExternalTool(format!(
                        "{title}失败，退出码 {}：{}",
                        output.exit_code, output.stderr
                    )));
                }

                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot};
    use nwflash_windows::{ProcessExecutor, ProcessOutput};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    fn adb(serial: &str) -> DeviceSnapshot {
        DeviceSnapshot {
            connection_state: DeviceConnectionState::AdbConnected,
            serial: serial.to_string(),
            connection_label: "ADB 已连接".to_string(),
            model: "--".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        }
    }

    #[test]
    fn reboot_plan_accepts_only_the_three_explicit_targets() {
        assert_eq!(
            build_reboot_command("SN-1", DeviceRebootTarget::System)
                .unwrap()
                .args,
            vec!["-s", "SN-1", "reboot"]
        );
        assert_eq!(
            build_reboot_command("SN-1", DeviceRebootTarget::Bootloader)
                .unwrap()
                .args,
            vec!["-s", "SN-1", "reboot", "bootloader"]
        );
        assert_eq!(
            build_reboot_command("SN-1", DeviceRebootTarget::Fastboot)
                .unwrap()
                .args,
            vec!["-s", "SN-1", "reboot", "fastboot"]
        );
    }

    #[test]
    fn fastboot_reboot_plan_uses_fastboot_for_a_fastboot_connected_device() {
        let command = build_reboot_command_for_connection(
            "FB-1",
            DeviceConnectionState::FastbootConnected,
            DeviceRebootTarget::Bootloader,
        )
        .expect("fastboot reboot plan should build");

        assert_eq!(
            command.program,
            nwflash_windows::bundled_platform_tool("fastboot.exe")
        );
        assert_eq!(command.args, vec!["-s", "FB-1", "reboot", "bootloader"]);
    }

    #[test]
    fn manual_snapshot_update_requests_an_event_even_when_identity_is_unchanged() {
        let runtime = DeviceRuntime::new();

        let update = runtime.apply_snapshot(adb("SN-1"), false, DeviceRefreshMode::Manual);

        assert_eq!(update.snapshot, adb("SN-1"));
        assert!(update.should_emit);
    }

    #[test]
    fn reboot_serial_is_available_only_for_the_current_adb_connection() {
        let runtime = DeviceRuntime::new();
        runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::FastbootConnected,
                serial: "FAST-1".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );

        let error = runtime
            .active_adb_serial()
            .expect_err("fastboot must not be rebooted through adb");
        assert!(error.contains("ADB"));
    }

    #[test]
    fn fastboot_serial_is_available_only_for_the_current_fastboot_connection() {
        let runtime = DeviceRuntime::new();
        runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::FastbootConnected,
                serial: "FAST-1".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );

        assert_eq!(runtime.active_fastboot_serial().unwrap(), "FAST-1");
    }

    #[test]
    fn automatic_discovery_error_is_projected_without_exposing_process_details() {
        let snapshot = discovery_error_snapshot();

        assert_eq!(snapshot.connection_state, DeviceConnectionState::Error);
        assert_eq!(snapshot.serial, "--");
        assert_eq!(snapshot.connection_label, "设备检测失败");
    }

    #[test]
    fn adb_information_enrichment_projects_only_the_overview_fields() {
        let snapshot = enrich_adb_snapshot(
            adb("RF8T123"),
            "[ro.product.model]: [V2318A]\n[ro.build.version.release]: [15]\n",
            "level: 78\n",
        );

        assert_eq!(snapshot.model, "V2318A");
        assert_eq!(snapshot.android_version, "15");
        assert_eq!(snapshot.battery_level, "78%");
    }

    #[tokio::test]
    async fn automatic_refresh_is_skipped_without_mutating_snapshot_while_operation_is_busy() {
        let runtime = DeviceRuntime::new();
        let original = adb("SN-BUSY");
        runtime.apply_snapshot(original.clone(), false, DeviceRefreshMode::Manual);

        let update = automatic_device_refresh(&runtime, true).await;

        assert_eq!(update.snapshot, original);
        assert!(!update.should_emit);
    }

    #[test]
    fn refresh_gate_denies_discovery_for_flashing_and_teardown_admission() {
        use nwflash_application::OperationAdmissionState;

        assert_eq!(
            device_refresh_block_reason(OperationAdmissionState::Running, OperationKind::Flashing,),
            Some("denied:flashing")
        );
        assert_eq!(
            device_refresh_block_reason(OperationAdmissionState::ExitPending, OperationKind::Idle,),
            Some("skipped:exit_pending")
        );
        assert_eq!(
            device_refresh_block_reason(OperationAdmissionState::Terminating, OperationKind::Idle,),
            Some("skipped:terminating")
        );
        assert!(device_refresh_is_blocked(
            OperationAdmissionState::Running,
            OperationKind::Flashing,
        ));
        assert!(device_refresh_is_blocked(
            OperationAdmissionState::ExitPending,
            OperationKind::Idle,
        ));
        assert!(device_refresh_is_blocked(
            OperationAdmissionState::Terminating,
            OperationKind::Idle,
        ));
        assert!(!device_refresh_is_blocked(
            OperationAdmissionState::Running,
            OperationKind::Idle,
        ));
    }

    #[test]
    fn refresh_gate_records_only_a_safe_denied_or_skipped_reason() {
        let log = OperationLogStore::new(None, 10);

        record_refresh_gate(&log, "设备刷新", "skipped:exit_pending");

        let entries = log.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, OperationLogLevel::Warning);
        assert_eq!(
            entries[0].message,
            "设备刷新已跳过（skipped:exit_pending）。"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_discovery_spawn_rechecks_exit_pending_after_idle_admission() {
        let coordinator = nwflash_application::OperationCoordinator::default();
        let _idle = coordinator
            .try_acquire_idle()
            .expect("initial refresh admission should be idle");
        let reached_boundary = Arc::new(Barrier::new(2));
        let release_boundary = Arc::new(Barrier::new(2));
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let log = OperationLogStore::new(None, 10);

        let task = tokio::spawn({
            let coordinator = coordinator.clone();
            let reached_boundary = reached_boundary.clone();
            let release_boundary = release_boundary.clone();
            let spawn_count = spawn_count.clone();
            let log = log.clone();
            async move {
                discover_current_device_guarded_with(
                    &coordinator,
                    Some(&log),
                    "设备刷新",
                    move || {
                        reached_boundary.wait();
                        release_boundary.wait();
                    },
                    move || {
                        spawn_count.fetch_add(1, Ordering::SeqCst);
                        Ok(DeviceSnapshot::disconnected())
                    },
                )
                .await
            }
        });

        tokio::task::spawn_blocking(move || reached_boundary.wait())
            .await
            .expect("boundary waiter should finish");
        assert_eq!(
            coordinator.request_exit_pending(),
            OperationAdmissionState::ExitPending
        );
        tokio::task::spawn_blocking(move || release_boundary.wait())
            .await
            .expect("boundary release should finish");

        let error = task
            .await
            .expect("guarded refresh task should join")
            .expect_err("exit-pending refresh must stop before executor spawn");
        assert_eq!(error.admission_reason(), Some("skipped:exit_pending"));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        let entries = log.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].message,
            "设备刷新已跳过（skipped:exit_pending）。"
        );
    }

    #[tokio::test]
    async fn manual_discovery_failure_never_returns_or_logs_raw_process_output() {
        let coordinator = nwflash_application::OperationCoordinator::default();
        let _idle = coordinator
            .try_acquire_idle()
            .expect("initial refresh admission should be idle");
        let log = OperationLogStore::new(None, 10);
        let sentinel =
            "Bearer SECRET SERIAL-PRIVATE C:\\Users\\mi\\secret https://private.invalid/ota";

        let error = discover_current_device_guarded_with(
            &coordinator,
            Some(&log),
            "设备刷新",
            || {},
            move || Err(DomainError::ExternalTool(sentinel.to_string())),
        )
        .await
        .expect_err("external discovery failure should be safely categorized");

        assert_eq!(
            error.public_message(),
            "设备检测失败，请检查设备连接后重试。"
        );
        let rendered = format!("{error:?} {}", error.public_message());
        assert!(!rendered.contains("SECRET"));
        assert!(!rendered.contains("SERIAL-PRIVATE"));
        assert!(!rendered.contains("Users"));
        assert!(!rendered.contains("private.invalid"));
        let entries = log.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "设备刷新失败（failed:discovery）。");
        assert!(!entries[0].message.contains("SECRET"));
    }

    #[derive(Clone)]
    struct CountingProcessExecutor {
        spawn_count: Arc<AtomicUsize>,
    }

    impl ProcessExecutor for CountingProcessExecutor {
        fn run(&self, _command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
            self.spawn_count.fetch_add(1, Ordering::SeqCst);
            Ok(ProcessOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn process_executor_rechecks_admission_at_the_actual_run_boundary() {
        let coordinator = OperationCoordinator::default();
        let _idle = coordinator
            .try_acquire_idle()
            .expect("initial process admission should be idle");
        let reached_boundary = Arc::new(Barrier::new(2));
        let release_boundary = Arc::new(Barrier::new(2));
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let executor = AdmissionCheckedExecutor::with_hook(
            coordinator.clone(),
            CountingProcessExecutor {
                spawn_count: spawn_count.clone(),
            },
            {
                let reached_boundary = reached_boundary.clone();
                let release_boundary = release_boundary.clone();
                move || {
                    reached_boundary.wait();
                    release_boundary.wait();
                }
            },
        );

        let run = std::thread::spawn(move || {
            executor.run(ProcessCommand::new("unused", Vec::<String>::new()))
        });
        reached_boundary.wait();
        assert_eq!(
            coordinator.request_exit_pending(),
            OperationAdmissionState::ExitPending
        );
        release_boundary.wait();

        let error = run
            .join()
            .expect("executor thread should join")
            .expect_err("exit-pending executor must refuse the process call");
        assert!(matches!(error, DomainError::AuthorizationDenied(_)));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        let rendered = error.to_string();
        assert!(rendered.contains("skipped:exit_pending"));
        assert!(!rendered.contains("unused"));
    }

    #[tokio::test]
    async fn actual_executor_admission_denial_is_not_reclassified_as_discovery_failure() {
        let coordinator = OperationCoordinator::default();
        let _idle = coordinator
            .try_acquire_idle()
            .expect("initial process admission should be idle");
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let executor = AdmissionCheckedExecutor::with_hook(
            coordinator.clone(),
            CountingProcessExecutor {
                spawn_count: spawn_count.clone(),
            },
            {
                let coordinator = coordinator.clone();
                move || {
                    coordinator.request_exit_pending();
                }
            },
        );
        let log = OperationLogStore::new(None, 10);

        let error = discover_current_device_guarded_with(
            &coordinator,
            Some(&log),
            "设备自动刷新",
            || {},
            move || {
                executor.run(ProcessCommand::new("unused", Vec::<String>::new()))?;
                Ok(DeviceSnapshot::disconnected())
            },
        )
        .await
        .expect_err("actual executor denial must remain an admission outcome");

        assert_eq!(error.admission_reason(), Some("skipped:exit_pending"));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        let entries = log.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].message,
            "设备自动刷新已跳过（skipped:exit_pending）。"
        );
    }

    #[test]
    fn automatic_actual_executor_denial_preserves_the_authoritative_snapshot() {
        let runtime = DeviceRuntime::new();
        let original = adb("SN-AUTHORITATIVE");
        runtime.apply_snapshot(original.clone(), false, DeviceRefreshMode::Manual);

        let update = project_automatic_discovery_result(
            &runtime,
            Err(DeviceDiscoveryFailure::Admission("skipped:exit_pending")),
        );

        assert_eq!(update.snapshot, original);
        assert!(!update.should_emit);
    }
}
