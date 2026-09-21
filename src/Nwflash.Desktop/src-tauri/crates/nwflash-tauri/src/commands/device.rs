use std::sync::{Arc, Mutex};

use nwflash_application::{
    apply_fastboot_device_details, fastboot_details_seed, parse_adb_battery_level,
    parse_adb_device_details, parse_kernel_version, result_to_domain_error, DeviceMonitor,
    DeviceSession, MonitorRefreshResult, OperationAdmissionState, OperationCommandRecorder,
    OperationCoordinator,
};
use nwflash_domain::{
    DeviceConnectionState, DeviceDetailsSnapshot, DeviceRefreshMode, DeviceSnapshot, DomainError,
    OperationKind, OperationLogLevel,
};
use nwflash_infrastructure::OperationLogStore;
use nwflash_windows::{
    process::{run_command, run_command_with_cancel_recorded},
    DeviceTransport, PlatformDeviceDiscovery, PlatformTools, ProcessCommand, ProcessExecutor,
    ProcessOutput, SystemProcessExecutor,
};
use serde::Serialize;
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
    /// 是否要把本轮结果广播给前端。补偿刷新会强制置真，即使设备身份未变。
    pub should_emit: bool,
    /// 设备身份（连接态 + 序列号）是否真的变了。下游 reconcile（镜像自动启动）
    /// 只认这个信号：心跳每 3 秒跑一次，按 should_emit 触发会让 scrcpy
    /// 被反复重启。
    pub identity_changed: bool,
}

/// 设备概览的一次完整投影：连接快照（连接态/序列号/型号/系统版本/电量）
/// 加上设备档案（槽位/引导加载器/内核/验证启动）。
///
/// ADB 与 Fastboot 两种连接态共用同一份载荷，前端不需要按连接态分支取值；
/// 设备/工具给不出的值统一落到 `--`，内部占位符（`Not available`）不外泄。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceOverviewPayload {
    pub connection_state: DeviceConnectionState,
    pub serial: String,
    pub connection_label: String,
    pub model: String,
    pub android_version: String,
    pub battery_level: String,
    pub active_slot: String,
    pub bootloader_state: String,
    pub kernel_version: String,
    pub verified_boot_state: String,
}

impl DeviceOverviewPayload {
    pub fn new(snapshot: &DeviceSnapshot, details: &DeviceDetailsSnapshot) -> Self {
        Self {
            connection_state: snapshot.connection_state,
            serial: snapshot.serial.clone(),
            connection_label: snapshot.connection_label.clone(),
            model: snapshot.model.clone(),
            android_version: snapshot.android_version.clone(),
            battery_level: snapshot.battery_level.clone(),
            active_slot: display_slot(&details.active_slot),
            bootloader_state: display_bootloader_state(&details.bootloader_state),
            kernel_version: display_text(&details.kernel_version),
            verified_boot_state: display_verified_boot_state(&details.verified_boot_state),
        }
    }
}

fn display_text(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value == "--" || value == "Not available" || value == "未检测到设备" {
        "--".to_string()
    } else {
        value.to_string()
    }
}

fn display_slot(value: &str) -> String {
    display_text(value.trim().trim_start_matches('_'))
}

fn display_bootloader_state(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "unlocked" => "已解锁".to_string(),
        "locked" => "已锁定".to_string(),
        _ => "--".to_string(),
    }
}

fn display_verified_boot_state(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "green" => "已校验".to_string(),
        "orange" => "未校验".to_string(),
        "yellow" => "自定义".to_string(),
        _ => display_text(value),
    }
}

/// 一次设备发现的完整结果：连接快照 + 设备档案。
#[derive(Debug)]
pub(crate) struct DeviceDiscoveryOutcome {
    pub snapshot: DeviceSnapshot,
    pub details: DeviceDetailsSnapshot,
}

#[derive(Clone)]
pub struct DeviceRuntime {
    monitor: Arc<Mutex<DeviceMonitor>>,
    // 最近一次被接受的设备档案（槽位/引导加载器/内核/验证启动）。与连接快照
    // 分开存放：连接快照带 busy 抑制与身份去抖，档案只是投影用的补充信息，
    // 不参与连接态判定。只在发现被接受时一起落盘，避免出现「快照还是旧设备、
    // 档案已清空」的中间态。
    details: Arc<Mutex<DeviceDetailsSnapshot>>,
    // 独立的刷新互斥（try-lock 即跳过），对应 C# DeviceMonitorService.refreshGate：
    // 手动刷新与自动心跳绝不并发跑 adb，也不占用全局操作门。
    // 用 tokio 互斥（guard 为 Send）：guard 需要跨命令 future 的
    // `.await` 存活，std MutexGuard 非 Send 会让 tauri::spawn 拒绝命令。
    refresh_gate: Arc<tokio::sync::Mutex<()>>,
    // 连续自动发现失败计数：用于日志节流（首次 Warning、第三次 Error、
    // 其余静默），对应 C# DeviceSessionService 的失败记录节奏。
    automatic_refresh_failures: Arc<std::sync::atomic::AtomicU32>,
    // 有过一轮刷新被跳过（任务占用 / 退出中 / 刷新互斥被占）：下一次成功发现
    // 必须强制广播。否则前端可能永远停在跳过期间看到的旧状态——例如任务
    // 结束后设备概览仍显示「未检测到设备」。
    pending_broadcast: Arc<std::sync::atomic::AtomicBool>,
}

/// `refresh_gate` 的独占 guard。guard 本身借用互斥锁，而调用方需要把
/// 它跨 `.await` 存进命令 future，故把同锁 `Arc` 与 guard 存在一起：
/// Arc 保证互斥锁活得比 guard 久（安全前提），'static 生命周期延展
/// 仅为通过所有权检查，tokio MutexGuard 本身满足 Send。
pub struct RefreshGuard {
    _gate: Arc<tokio::sync::Mutex<()>>,
    // 持有即上锁；字段本身不被读取，为绕过 dead_code 加前缀并保留说明。
    #[allow(dead_code)]
    locked: tokio::sync::MutexGuard<'static, ()>,
}

impl RefreshGuard {
    fn try_acquire(gate: &Arc<tokio::sync::Mutex<()>>) -> Option<Self> {
        let locked = gate.try_lock().ok()?;
        // 安全性：guard 释放（Drop）必然先于 _gate 的 Arc 解构，
        // 字段声明顺序保证 drop 顺序为 locked → _gate，不存在悬垂。
        fn extend<'a>(
            guard: tokio::sync::MutexGuard<'a, ()>,
        ) -> tokio::sync::MutexGuard<'static, ()> {
            unsafe { std::mem::transmute(guard) }
        }
        Some(Self {
            _gate: Arc::clone(gate),
            locked: extend(locked),
        })
    }
}

impl DeviceRuntime {
    pub fn new() -> Self {
        Self {
            monitor: Arc::new(Mutex::new(DeviceMonitor::new(
                DeviceSnapshot::disconnected(),
            ))),
            details: Arc::new(Mutex::new(DeviceDetailsSnapshot::empty())),
            refresh_gate: Arc::new(tokio::sync::Mutex::new(())),
            automatic_refresh_failures: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            pending_broadcast: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// 记录一次被跳过的刷新：下一轮成功发现必须强制广播一次。
    pub fn note_refresh_skipped(&self) {
        self.pending_broadcast
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 取走并清空「必须补一次广播」标记。
    fn take_pending_broadcast(&self) -> bool {
        self.pending_broadcast
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    /// 自动发现失败的日志节流：返回本条失败是否应写日志及级别。
    /// 首次失败 Warning、第三次 Error、其余静默；成功后计数清零。
    fn note_automatic_refresh_failure(&self) -> (bool, OperationLogLevel) {
        use std::sync::atomic::Ordering;
        let count = self
            .automatic_refresh_failures
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        match count {
            1 => (true, OperationLogLevel::Warning),
            3 => (true, OperationLogLevel::Error),
            _ => (false, OperationLogLevel::Warning),
        }
    }

    fn reset_automatic_refresh_failures(&self) {
        self.automatic_refresh_failures
            .store(0, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn apply_snapshot(
        &self,
        snapshot: DeviceSnapshot,
        is_device_busy: bool,
        mode: DeviceRefreshMode,
    ) -> DeviceSnapshotUpdate {
        self.apply_discovery(snapshot, None, is_device_busy, mode)
    }

    /// 落盘一次发现结果。档案只在连接快照被接受（未 busy 跳过、未被去抖延迟）
    /// 时同步更新，避免「快照仍是旧设备、档案已换新」的中间态。
    pub fn apply_discovery(
        &self,
        snapshot: DeviceSnapshot,
        details: Option<DeviceDetailsSnapshot>,
        is_device_busy: bool,
        mode: DeviceRefreshMode,
    ) -> DeviceSnapshotUpdate {
        let mut monitor = self
            .monitor
            .lock()
            .expect("device monitor lock should not be poisoned");
        let previous = monitor.snapshot().clone();
        let result = monitor.refresh(snapshot, is_device_busy, mode);
        let next = monitor.snapshot().clone();
        let accepted = matches!(
            result,
            MonitorRefreshResult::Applied | MonitorRefreshResult::AppliedAndBroadcast
        );
        let identity_changed = accepted
            && (previous.connection_state != next.connection_state
                || previous.serial != next.serial);
        drop(monitor);

        if accepted {
            if let Some(details) = details {
                self.set_details(details);
            }
        }

        DeviceSnapshotUpdate {
            snapshot: next,
            should_emit: matches!(result, MonitorRefreshResult::AppliedAndBroadcast),
            identity_changed,
        }
    }

    pub fn set_details(&self, details: DeviceDetailsSnapshot) {
        *self
            .details
            .lock()
            .expect("device details lock should not be poisoned") = details;
    }

    pub fn details(&self) -> DeviceDetailsSnapshot {
        self.details
            .lock()
            .expect("device details lock should not be poisoned")
            .clone()
    }

    /// 当前权威的设备概览载荷（连接快照 + 设备档案）。
    pub fn overview_payload(&self) -> DeviceOverviewPayload {
        DeviceOverviewPayload::new(&self.snapshot(), &self.details())
    }

    /// 尝试独占本轮设备刷新；已有一轮刷新在跑时返回 None（跳过本轮），
    /// 与 C# `refreshGate.WaitAsync(0)` 的“不排队直接跳过”语义一致。
    pub fn try_begin_refresh(&self) -> Option<RefreshGuard> {
        RefreshGuard::try_acquire(&self.refresh_gate)
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
            identity_changed: false,
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
    automatic_device_refresh_guarded_with_options(runtime, coordinator, operation_log_store, false)
        .await
}

/// 补偿刷新（操作 busy→idle 沿触发）：即使设备身份未变也强制广播，
/// 对应 C# `RefreshAutomaticallyAsync` 的 `forceFire: true`——刷写完成后
/// 下游（分区表等）必须重读，不能因“序列号没变”被吞掉。
pub async fn compensating_device_refresh_guarded_with_log(
    runtime: &DeviceRuntime,
    coordinator: &OperationCoordinator,
    operation_log_store: Option<&OperationLogStore>,
) -> DeviceSnapshotUpdate {
    automatic_device_refresh_guarded_with_options(runtime, coordinator, operation_log_store, true)
        .await
}

async fn automatic_device_refresh_guarded_with_options(
    runtime: &DeviceRuntime,
    coordinator: &OperationCoordinator,
    operation_log_store: Option<&OperationLogStore>,
    force_broadcast: bool,
) -> DeviceSnapshotUpdate {
    // 自动刷新绝不占用全局操作门（C# 参考只做只读 IsBusy 检查）：
    // 占门会产生“每 3 秒随机拒绝用户操作”的假忙窗口，发现命令挂死时
    // 还会演变为无法恢复的全局死锁。
    let operation = if coordinator.is_busy() {
        OperationKind::Flashing
    } else {
        OperationKind::Idle
    };
    if let Some(reason) = device_refresh_block_reason(coordinator.admission_state(), operation) {
        // busy 期间的跳过是常态（C# 参考静默返回），不写日志防刷屏；
        // 退出/终止态的跳过仍记录一次。
        if reason != "denied:flashing" {
            if let Some(log) = operation_log_store {
                record_refresh_gate(log, "设备自动刷新", reason);
            }
        }
        // 跳过意味着前端这轮拿不到新状态：记下来，等任务结束后的第一次成功
        // 发现强制广播一次，否则设备概览会停在跳过期间看到的旧状态。
        runtime.note_refresh_skipped();
        return DeviceSnapshotUpdate {
            snapshot: runtime.snapshot(),
            should_emit: false,
            identity_changed: false,
        };
    }

    // 独立刷新互斥：手动刷新/上一轮心跳在跑时直接跳过本轮，不并发跑 adb。
    // guard 只覆盖 discovery 这一段 await;project 阶段在锁外做,否则
    // apply_snapshot 要等同一把 runtime Mutex 且持有 guard 的 future 不 Send。
    let Some(refresh_guard) = runtime.try_begin_refresh() else {
        runtime.note_refresh_skipped();
        return DeviceSnapshotUpdate {
            snapshot: runtime.snapshot(),
            should_emit: false,
            identity_changed: false,
        };
    };

    let known_details = runtime.details();
    let result = discover_current_device_guarded_with_log_policy(
        coordinator,
        operation_log_store,
        "设备自动刷新",
        || {},
        {
            let coordinator = coordinator.clone();
            move || discover_current_device_blocking_guarded(coordinator, known_details)
        },
        || runtime.note_automatic_refresh_failure(),
    )
    .await;
    drop(refresh_guard);
    if result.is_ok() {
        runtime.reset_automatic_refresh_failures();
    }
    // 准入类失败（退出中/终止中）说明本轮根本没读设备，不能消费补偿标记；
    // 其余情况（读到设备或发现失败）都已经把运行时状态刷新过一次，
    // 需要就把「跳过期间欠下的那次广播」还掉。
    let admission_skip = result
        .as_ref()
        .err()
        .is_some_and(|error| error.admission_reason().is_some());
    let mut update = project_automatic_discovery_result(runtime, result);
    if force_broadcast || (!admission_skip && runtime.take_pending_broadcast()) {
        update.should_emit = true;
    }
    update
}

fn project_automatic_discovery_result(
    runtime: &DeviceRuntime,
    result: Result<DeviceDiscoveryOutcome, DeviceDiscoveryFailure>,
) -> DeviceSnapshotUpdate {
    match result {
        Ok(outcome) => runtime.apply_discovery(
            outcome.snapshot,
            Some(outcome.details),
            false,
            DeviceRefreshMode::Automatic,
        ),
        Err(error) if error.admission_reason().is_some() => DeviceSnapshotUpdate {
            snapshot: runtime.snapshot(),
            should_emit: false,
            identity_changed: false,
        },
        Err(_) => {
            let update = runtime.apply_snapshot(
                discovery_error_snapshot(),
                false,
                DeviceRefreshMode::Automatic,
            );
            // 只有错误快照真的被接受（未被去抖延迟）时才清空档案，
            // 否则会出现「快照仍是已连接设备、档案已被清空」的中间态。
            if update.snapshot.connection_state == DeviceConnectionState::Error {
                runtime.set_details(DeviceDetailsSnapshot::empty());
            }
            update
        }
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

async fn discover_current_device_guarded_with<BeforeSpawn, Discover>(
    coordinator: &OperationCoordinator,
    operation_log_store: Option<&OperationLogStore>,
    operation: &'static str,
    before_spawn: BeforeSpawn,
    discover: Discover,
) -> Result<DeviceDiscoveryOutcome, DeviceDiscoveryFailure>
where
    BeforeSpawn: FnOnce() + Send + 'static,
    Discover: FnOnce() -> Result<DeviceDiscoveryOutcome, DomainError> + Send + 'static,
{
    let result = discover_current_device_guarded_with_log_policy(
        coordinator,
        operation_log_store,
        operation,
        before_spawn,
        discover,
        || (true, OperationLogLevel::Warning),
    )
    .await;
    result
}

/// `log_policy` 在非准入类失败时被调用，决定本条失败是否写日志及级别。
/// 自动刷新传入节流策略（首次 Warning / 第三次 Error / 其余静默），
/// 手动刷新保持每条都记。
async fn discover_current_device_guarded_with_log_policy<BeforeSpawn, Discover, LogPolicy>(
    coordinator: &OperationCoordinator,
    operation_log_store: Option<&OperationLogStore>,
    operation: &'static str,
    before_spawn: BeforeSpawn,
    discover: Discover,
    log_policy: LogPolicy,
) -> Result<DeviceDiscoveryOutcome, DeviceDiscoveryFailure>
where
    BeforeSpawn: FnOnce() + Send + 'static,
    Discover: FnOnce() -> Result<DeviceDiscoveryOutcome, DomainError> + Send + 'static,
    LogPolicy: FnOnce() -> (bool, OperationLogLevel),
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
            let (should_log, level) = log_policy();
            if should_log {
                log.write(
                    level,
                    format!("{operation}失败（{}）。", error.log_reason()),
                    None,
                );
            }
        }
    }
    result
}

pub(crate) async fn discover_current_device() -> Result<DeviceSnapshot, String> {
    let known = DeviceDetailsSnapshot::empty();
    task::spawn_blocking(move || discover_current_device_blocking(&known))
        .await
        .map_err(|_| DEVICE_DISCOVERY_PUBLIC_ERROR.to_string())?
        .map(|outcome| outcome.snapshot)
        .map_err(|_| DEVICE_DISCOVERY_PUBLIC_ERROR.to_string())
}

fn discover_current_device_blocking(
    known_details: &DeviceDetailsSnapshot,
) -> Result<DeviceDiscoveryOutcome, DomainError> {
    let tools = PlatformTools::bundled();
    let discovery = PlatformDeviceDiscovery::new(tools.clone());
    let snapshot = DeviceSession::refresh(&discovery)?;
    if snapshot.connection_state == DeviceConnectionState::FastbootConnected {
        let seed = fastboot_details_seed(known_details, &snapshot.serial);
        let details = fastboot_device_details(seed, &tools, |command| {
            match command.and_then(run_command) {
                Ok(output) if output.exit_code == 0 => Ok(output.stdout),
                Ok(_) | Err(_) => Ok(String::new()),
            }
        })?;
        let snapshot = apply_fastboot_details(snapshot, &details);
        return Ok(DeviceDiscoveryOutcome { snapshot, details });
    }
    if snapshot.connection_state != DeviceConnectionState::AdbConnected {
        return Ok(DeviceDiscoveryOutcome {
            snapshot,
            details: DeviceDetailsSnapshot::empty(),
        });
    }

    let transport = DeviceTransport::new(tools);
    let properties = readonly_stdout(transport.build_adb_getprop_command(&snapshot.serial));
    let battery = readonly_stdout(transport.build_adb_battery_command(&snapshot.serial));
    let kernel = readonly_stdout(transport.build_adb_kernel_version_command(&snapshot.serial));
    let details = adb_device_details(&snapshot.serial, &properties, &kernel);
    let snapshot = apply_adb_details(snapshot, &details, &battery);
    Ok(DeviceDiscoveryOutcome { snapshot, details })
}

fn discover_current_device_blocking_guarded(
    coordinator: OperationCoordinator,
    known_details: DeviceDetailsSnapshot,
) -> Result<DeviceDiscoveryOutcome, DomainError> {
    let tools = PlatformTools::bundled();
    let executor =
        AdmissionCheckedExecutor::new(coordinator, OperationKind::Idle, SystemProcessExecutor);
    let discovery = PlatformDeviceDiscovery::with_executor(tools.clone(), executor.clone());
    let snapshot = DeviceSession::refresh(&discovery)?;
    if snapshot.connection_state == DeviceConnectionState::FastbootConnected {
        let seed = fastboot_details_seed(&known_details, &snapshot.serial);
        let executor_for_details = executor.clone();
        let details = fastboot_device_details(seed, &tools, move |command| {
            match command.and_then(|command| executor_for_details.run(command)) {
                Ok(output) if output.exit_code == 0 => Ok(output.stdout),
                Ok(_) => Ok(String::new()),
                Err(error) if admission_reason_from_domain_error(&error).is_some() => Err(error),
                Err(_) => Ok(String::new()),
            }
        })?;
        let snapshot = apply_fastboot_details(snapshot, &details);
        return Ok(DeviceDiscoveryOutcome { snapshot, details });
    }
    if snapshot.connection_state != DeviceConnectionState::AdbConnected {
        return Ok(DeviceDiscoveryOutcome {
            snapshot,
            details: DeviceDetailsSnapshot::empty(),
        });
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
    let kernel = readonly_stdout_with_executor(
        transport.build_adb_kernel_version_command(&snapshot.serial),
        &executor,
    )?;
    let details = adb_device_details(&snapshot.serial, &properties, &kernel);
    let snapshot = apply_adb_details(snapshot, &details, &battery);
    Ok(DeviceDiscoveryOutcome { snapshot, details })
}

/// ADB 连接态的档案读取：属性、内核版本都来自本轮已经拿到的只读输出，
/// 不额外起进程读同一份数据。
fn adb_device_details(serial: &str, properties: &str, kernel: &str) -> DeviceDetailsSnapshot {
    let mut details = parse_adb_device_details(serial, properties);
    details.kernel_version = parse_kernel_version(kernel);
    details
}

fn apply_adb_details(
    mut snapshot: DeviceSnapshot,
    details: &DeviceDetailsSnapshot,
    battery: &str,
) -> DeviceSnapshot {
    snapshot.model = details.model.clone();
    snapshot.android_version = details.android_version.clone();
    snapshot.battery_level = parse_adb_battery_level(battery);
    snapshot
}

/// Fastboot 设备档案：先沿用同一台设备上一次读到的档案（对应 C#
/// `DeviceSessionService.IsSameDevice`），再读取 current-slot / unlocked /
/// product getvar 覆盖。可选变量失败降级为“未读取”，不让整个刷新抛错
/// （对应 C# DeviceInfoService.ReadFastbootAsync）。
fn fastboot_device_details<E>(
    seed: DeviceDetailsSnapshot,
    tools: &PlatformTools,
    mut read_variable: E,
) -> Result<DeviceDetailsSnapshot, DomainError>
where
    E: FnMut(Result<ProcessCommand, DomainError>) -> Result<String, DomainError>,
{
    let serial = seed.serial.clone();
    let transport = DeviceTransport::new(tools.clone());
    let current_slot = extract_fastboot_variable_value(&read_variable(
        transport.build_fastboot_getvar_command(&serial, "current-slot"),
    )?);
    let unlocked = extract_fastboot_variable_value(&read_variable(
        transport.build_fastboot_getvar_command(&serial, "unlocked"),
    )?);
    let product = extract_fastboot_variable_value(&read_variable(
        transport.build_fastboot_getvar_command(&serial, "product"),
    )?);

    Ok(apply_fastboot_device_details(
        seed,
        &current_slot,
        &unlocked,
        &product,
    ))
}

/// Fastboot 连接态的快照投影：型号用 `product` 补齐，连接标签附上引导加载器
/// 锁定态。槽位不再塞进 `android_version`——槽位有独立字段，而 fastboot 下
/// 系统版本确实读不到，保持 `--` 才是诚实的。
fn apply_fastboot_details(
    mut snapshot: DeviceSnapshot,
    details: &DeviceDetailsSnapshot,
) -> DeviceSnapshot {
    if !is_unavailable_value(&details.model) {
        snapshot.model = details.model.clone();
    } else {
        // `fastboot getvar product` 失败时型号确实读不到，但设备就在 fastboot 里：
        // 不能沿用「未检测到设备」——那会把「读不到型号」说成「没有设备」。
        snapshot.model = "Fastboot 设备".to_string();
    }
    if details.bootloader_state == "unlocked" || details.bootloader_state == "locked" {
        let state_label = if details.bootloader_state == "unlocked" {
            "已解锁"
        } else {
            "已锁定"
        };
        snapshot.connection_label = format!(
            "{}（Bootloader {}）",
            snapshot.connection_label, state_label
        );
    }
    snapshot
}

/// 从 fastboot getvar 输出提取变量值（剥离 `(bootloader)` 前缀），
/// 读取失败/空输出时返回空串，由上层按“未读取”降级。
fn extract_fastboot_variable_value(output: &str) -> String {
    for source_line in output.lines() {
        let line = source_line.trim();
        let line = line
            .strip_prefix("(bootloader)")
            .unwrap_or(line)
            .trim_start();
        let line = line.strip_prefix("INFO").unwrap_or(line).trim_start();
        if let Some((_, value)) = line.rsplit_once(':') {
            let value = value.trim();
            if !value.is_empty() && !value.eq_ignore_ascii_case("yes command failed") {
                return value.to_string();
            }
        }
    }
    String::new()
}

fn is_unavailable_value(value: &str) -> bool {
    value.trim().is_empty() || matches!(value, "--" | "Not available" | "未检测到设备")
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
) -> Result<DeviceOverviewPayload, String> {
    // 任务进行中（刷写/传输/安装/哈希/投屏/重启）一律不做发现：对应 C#
    // DeviceMonitorService.RefreshCoreAsync 的 `if (coordinator.IsBusy) return`。
    // 此时设备多半正处在 adb↔fastboot↔系统的过渡态，跑发现会把「过渡态读不到
    // 设备」当成结论写进设备概览——主页偶尔显示「未检测到设备」就是这条路径。
    let operation = if state.operation_coordinator.is_busy() {
        OperationKind::Flashing
    } else {
        OperationKind::Idle
    };
    let admission = state.operation_coordinator.admission_state();
    if let Some(reason) = device_refresh_block_reason(admission, operation) {
        // busy 期间的跳过是常态（与自动刷新一致静默），退出/终止态仍记录一次。
        if reason != "denied:flashing" {
            record_refresh_gate(&state.operation_log_store, "设备刷新", reason);
        }
        state.device_runtime.note_refresh_skipped();
        // 返回当前权威快照而不是错误：前端要看到的是「上一次已知的真实设备」，
        // 而不是把「检测被暂停」渲染成设备检测失败。
        return Ok(state.device_runtime.overview_payload());
    }
    // 手动刷新同样不占用全局操作门；用与自动刷新共享的独立互斥
    // 防止并发跑 adb（C# refreshGate 对所有刷新入口生效）。
    // 把 guard 从 Option 中取出,discovery 结束即释放;reconcile 前必须
    // 已释放,自动镜像才能拿操作许可而不是在仍持刷新锁时以 InProgress 失败。
    let Some(refresh_guard) = state.device_runtime.try_begin_refresh() else {
        state.device_runtime.note_refresh_skipped();
        return Err("设备刷新正在进行中，请稍后重试。".to_string());
    };
    // 同一台设备上一次读到的档案：fastboot 分支要靠它把 ADB 侧读到的
    // 系统版本/内核/验证启动带过去（C# IsSameDevice 语义）。
    let known_details = state.device_runtime.details();
    let outcome = discover_current_device_guarded_with(
        &state.operation_coordinator,
        Some(&state.operation_log_store),
        "设备刷新",
        || {},
        {
            let coordinator = state.operation_coordinator.clone();
            move || discover_current_device_blocking_guarded(coordinator, known_details)
        },
    )
    .await
    .map_err(|error| error.public_message().to_string())?;
    drop(refresh_guard);

    let update = state.device_runtime.apply_discovery(
        outcome.snapshot,
        Some(outcome.details),
        state.operation_coordinator.is_busy(),
        DeviceRefreshMode::Manual,
    );
    // The discovery helper above takes no permits at all (it only reads the
    // admission state); the refresh guard was already released above, so the
    // mirror reconcile can take the operation permit instead of failing with
    // InProgress under a still-held refresh lock.
    let _ = crate::commands::mirror::reconcile_after_device_update(
        &state.mirror_runtime,
        &state.device_runtime,
        &state.operation_coordinator,
    )
    .await;
    let payload = state.device_runtime.overview_payload();
    if update.should_emit {
        app_handle
            .emit("device:snapshot", payload.clone())
            .map_err(|error| format!("设备状态事件发送失败：{error}"))?;
    }

    Ok(payload)
}

#[tauri::command]
pub async fn device_reboot_system(state: State<'_, AppState>) -> Result<(), String> {
    // 重启会打断刷机流程：入口第一行强制本地能力校验。
    crate::commands::guard::guard_write_command(&state)?;
    device_reboot(&state, DeviceRebootTarget::System).await
}

#[tauri::command]
pub async fn device_reboot_bootloader(state: State<'_, AppState>) -> Result<(), String> {
    // 重启会打断刷机流程：入口第一行强制本地能力校验。
    crate::commands::guard::guard_write_command(&state)?;
    device_reboot(&state, DeviceRebootTarget::Bootloader).await
}

#[tauri::command]
pub async fn device_reboot_fastboot(state: State<'_, AppState>) -> Result<(), String> {
    // 重启会打断刷机流程：入口第一行强制本地能力校验。
    crate::commands::guard::guard_write_command(&state)?;
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
                let timeout =
                    crate::command_timeout::for_command(&command, crate::command_timeout::CONTROL);
                let cancellation_for_command = cancellation.clone();
                // 逐条 adb/fastboot 命令只进上报服务器的使用日志 details。
                let command_recorder = OperationCommandRecorder::new(context.clone());
                let output = task::spawn_blocking(move || {
                    run_command_with_cancel_recorded(
                        command,
                        Some(timeout),
                        move || cancellation_for_command.is_cancelled(),
                        command_recorder,
                    )
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
        assert!(update.identity_changed);
    }

    #[test]
    fn repeated_same_identity_discovery_does_not_report_an_identity_change() {
        let runtime = DeviceRuntime::new();
        runtime.apply_snapshot(adb("SN-1"), false, DeviceRefreshMode::Manual);

        let update = runtime.apply_snapshot(adb("SN-1"), false, DeviceRefreshMode::Automatic);

        assert!(!update.identity_changed);
        assert!(!update.should_emit);
    }

    #[test]
    fn overview_payload_projects_device_details_for_the_ui() {
        let runtime = DeviceRuntime::new();
        let mut details = DeviceDetailsSnapshot::empty();
        details.active_slot = "_a".to_string();
        details.bootloader_state = "unlocked".to_string();
        details.kernel_version = "6.1.75-android14".to_string();
        details.verified_boot_state = "orange".to_string();
        runtime.set_details(details);

        let payload = runtime.overview_payload();

        assert_eq!(payload.active_slot, "a");
        assert_eq!(payload.bootloader_state, "已解锁");
        assert_eq!(payload.kernel_version, "6.1.75-android14");
        assert_eq!(payload.verified_boot_state, "未校验");
    }

    #[test]
    fn overview_payload_never_leaks_internal_placeholders() {
        let runtime = DeviceRuntime::new();
        let payload = runtime.overview_payload();

        assert_eq!(payload.active_slot, "--");
        assert_eq!(payload.bootloader_state, "--");
        assert_eq!(payload.kernel_version, "--");
        assert_eq!(payload.verified_boot_state, "--");
    }

    #[test]
    fn fastboot_details_read_slot_and_bootloader_state_from_getvars() {
        let variables = [
            ("current-slot", "b"),
            ("unlocked", "yes"),
            ("product", "PD2307"),
        ];
        let reads = Arc::new(AtomicUsize::new(0));

        let details = fastboot_device_details(
            fastboot_details_seed(&DeviceDetailsSnapshot::empty(), "FAST-1"),
            &PlatformTools::bundled(),
            {
                let reads = reads.clone();
                move |command| {
                    let command = command?;
                    assert_eq!(command.args[2], "getvar");
                    let variable = command.args[3].as_str();
                    let value = variables
                        .iter()
                        .find(|(name, _)| *name == variable)
                        .map(|(_, value)| (*value).to_string())
                        .unwrap_or_default();
                    reads.fetch_add(1, Ordering::SeqCst);
                    Ok(format!("(bootloader) {variable}: {value}\n"))
                }
            },
        )
        .expect("fastboot details should read");

        assert_eq!(reads.load(Ordering::SeqCst), 3);
        assert_eq!(details.serial, "FAST-1");
        assert_eq!(details.active_slot, "b");
        assert_eq!(details.bootloader_state, "unlocked");
        assert_eq!(details.model, "PD2307");
    }

    /// 设备从 ADB 重启进 fastboot 后，概览不能丢掉 ADB 侧已读到的档案：
    /// 系统版本/内核/验证启动必须沿用到 fastboot 快照里（C# IsSameDevice 语义）。
    #[test]
    fn fastboot_details_carry_known_adb_details_over_for_the_same_serial() {
        let mut known = DeviceDetailsSnapshot::empty();
        known.serial = "FAST-1".to_string();
        known.model = "V2318A".to_string();
        known.android_version = "15".to_string();
        known.kernel_version = "6.1.75-android14".to_string();
        known.verified_boot_state = "green".to_string();

        let details = fastboot_device_details(
            fastboot_details_seed(&known, "FAST-1"),
            &PlatformTools::bundled(),
            |_command| Ok(String::new()),
        )
        .expect("fastboot details should degrade without throwing");

        assert_eq!(details.serial, "FAST-1");
        assert_eq!(details.model, "V2318A");
        assert_eq!(details.android_version, "15");
        assert_eq!(details.kernel_version, "6.1.75-android14");
        assert_eq!(details.verified_boot_state, "green");
    }

    /// 换了一台设备：上一台的档案绝不能被带过去。
    #[test]
    fn fastboot_details_do_not_leak_a_different_devices_details() {
        let mut known = DeviceDetailsSnapshot::empty();
        known.serial = "OTHER-9".to_string();
        known.model = "V2318A".to_string();
        known.android_version = "15".to_string();

        let seed = fastboot_details_seed(&known, "FAST-1");

        assert_eq!(seed.serial, "FAST-1");
        assert_eq!(seed.model, "未检测到设备");
        assert_eq!(seed.android_version, "--");
    }

    #[test]
    fn fastboot_snapshot_keeps_android_version_empty_and_labels_the_bootloader() {
        let mut details = DeviceDetailsSnapshot::empty();
        details.active_slot = "b".to_string();
        details.bootloader_state = "unlocked".to_string();
        details.model = "PD2307".to_string();

        let snapshot = apply_fastboot_details(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::FastbootConnected,
                serial: "FAST-1".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "未检测到设备".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            &details,
        );

        assert_eq!(snapshot.model, "PD2307");
        // 槽位有独立字段，绝不能再冒充系统版本。
        assert_eq!(snapshot.android_version, "--");
        assert_eq!(
            snapshot.connection_label,
            "Fastboot 已连接（Bootloader 已解锁）"
        );
    }

    #[test]
    fn fastboot_snapshot_without_a_product_getvar_does_not_claim_no_device() {
        let snapshot = apply_fastboot_details(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::FastbootConnected,
                serial: "FAST-1".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "未检测到设备".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            &DeviceDetailsSnapshot::empty(),
        );

        assert_eq!(snapshot.model, "Fastboot 设备");
        assert_eq!(snapshot.serial, "FAST-1");
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
        let properties = "[ro.product.model]: [V2318A]\n[ro.build.version.release]: [15]\n\
            [ro.boot.slot_suffix]: [_a]\n[ro.boot.flash.locked]: [1]\n\
            [ro.boot.verifiedbootstate]: [green]\n";
        let details = adb_device_details("RF8T123", properties, "6.1.75-android14\n");
        let snapshot = apply_adb_details(adb("RF8T123"), &details, "level: 78\n");

        assert_eq!(snapshot.model, "V2318A");
        assert_eq!(snapshot.android_version, "15");
        assert_eq!(snapshot.battery_level, "78%");
        // 档案字段与连接快照分开存放：槽位/引导加载器/内核/验证启动不进快照。
        assert_eq!(details.active_slot, "a");
        assert_eq!(details.bootloader_state, "locked");
        assert_eq!(details.kernel_version, "6.1.75-android14");
        assert_eq!(details.verified_boot_state, "green");
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
                        Ok(DeviceDiscoveryOutcome {
                            snapshot: DeviceSnapshot::disconnected(),
                            details: DeviceDetailsSnapshot::empty(),
                        })
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
                Ok(DeviceDiscoveryOutcome {
                    snapshot: DeviceSnapshot::disconnected(),
                    details: DeviceDetailsSnapshot::empty(),
                })
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

    #[test]
    fn refresh_gate_skips_a_second_concurrent_refresh_and_never_holds_the_operation_gate() {
        // 独立刷新互斥（对应 C# refreshGate）：持锁期间第二轮直接跳过；
        // 自动/手动刷新都不占用全局操作门——门在空闲时必须仍可被抢占。
        let runtime = DeviceRuntime::new();
        let first = runtime
            .try_begin_refresh()
            .expect("an idle runtime should admit a refresh");
        assert!(
            runtime.try_begin_refresh().is_none(),
            "a second concurrent refresh must be skipped"
        );

        let coordinator = nwflash_application::OperationCoordinator::default();
        let idle = coordinator.try_acquire_idle();
        assert!(
            idle.is_ok(),
            "the operation gate must stay acquirable while a refresh is in flight"
        );
        drop(idle);
        drop(first);
        assert!(
            runtime.try_begin_refresh().is_some(),
            "releasing the refresh gate should admit the next refresh"
        );
    }

    #[test]
    fn automatic_refresh_failure_logging_is_throttled_to_first_and_third() {
        // 日志节流（对应 C# 首次 Warning / 第三次 Error / 其余静默）。
        let runtime = DeviceRuntime::new();
        assert_eq!(
            runtime.note_automatic_refresh_failure(),
            (true, OperationLogLevel::Warning)
        );
        assert_eq!(
            runtime.note_automatic_refresh_failure(),
            (false, OperationLogLevel::Warning)
        );
        assert_eq!(
            runtime.note_automatic_refresh_failure(),
            (true, OperationLogLevel::Error)
        );
        assert_eq!(
            runtime.note_automatic_refresh_failure(),
            (false, OperationLogLevel::Warning)
        );
        runtime.reset_automatic_refresh_failures();
        assert_eq!(
            runtime.note_automatic_refresh_failure(),
            (true, OperationLogLevel::Warning)
        );
    }
}
