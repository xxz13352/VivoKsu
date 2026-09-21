use std::{
    future::Future,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use nwflash_application::{
    result_to_domain_error, CommandSpec, MirrorService, OperationCoordinator,
};
use nwflash_domain::{DomainError, OperationKind};
use serde::Serialize;
use tauri::State;
use tokio::sync::{oneshot, Notify};
use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

use crate::{commands::device::DeviceRuntime, AppState};

/// 自动投屏异常退出的连续重启上限（对应 C# MirrorService.MaxConsecutiveRestartFailures）。
const MAX_CONSECUTIVE_MIRROR_RESTARTS: u32 = 3;
/// 异常退出后的重启延迟（对应 C# RestartAfterExitAsync 的 1 秒退避）。
const MIRROR_RESTART_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize)]
pub struct MirrorStatusDto {
    pub is_mirroring: bool,
    pub auto_mirror_enabled: bool,
}

#[derive(Default)]
struct MirrorRuntimeState {
    auto_mirror_enabled: bool,
    deliberate_stop: bool,
    child: Option<Child>,
    // scrcpy 异常退出的连续自动重启计数（对应 C# MaxConsecutiveRestartFailures）：
    // 自动投屏开启时崩溃应 1 秒后自动拉起，但连续失败要设上限，防止崩溃循环。
    consecutive_restart_failures: u32,
    /// stop() 终止失败时遗留的孤儿进程 PID（N16）：句柄已丢弃，只能在
    /// 下次 start 前按 PID 强杀，杜绝双实例。
    stale_pids: Vec<u32>,
}

#[derive(Clone, Default)]
pub struct MirrorRuntime {
    state: Arc<Mutex<MirrorRuntimeState>>,
    stop_notify: Arc<Notify>,
}

impl MirrorRuntime {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MirrorRuntimeState::default())),
            stop_notify: Arc::new(Notify::new()),
        }
    }

    pub fn status(&self) -> MirrorStatusDto {
        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        if state
            .child
            .as_mut()
            .is_some_and(|child| child.try_wait().ok().flatten().is_some())
        {
            state.child = None;
        }
        MirrorStatusDto {
            is_mirroring: state.child.is_some(),
            auto_mirror_enabled: state.auto_mirror_enabled,
        }
    }

    fn begin_manual_start(&self) {
        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.deliberate_stop = false;
        // 手动启动即新一轮投屏会话：清除上一会话残留的失败计数，
        // 否则 abandon 后残值 3 会让重启循环一次机会都不给就再 abandon。
        state.consecutive_restart_failures = 0;
    }

    fn set_auto_enabled(&self, enabled: bool) {
        if !enabled {
            self.state
                .lock()
                .expect("mirror state lock should not be poisoned")
                .auto_mirror_enabled = false;
            self.stop();
            return;
        }

        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.auto_mirror_enabled = true;
        state.deliberate_stop = false;
        // 重新打开自动投屏 = 用户重置恢复预期（对齐 C# ClearDeliberateStop
        // 同步复位 consecutiveRestartFailures）。
        state.consecutive_restart_failures = 0;
    }

    fn should_auto_start(&self) -> bool {
        let state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.auto_mirror_enabled && !state.deliberate_stop && state.child.is_none()
    }

    /// scrcpy 意外退出后是否应自动拉起（自动投屏开启且非用户主动停止）。
    fn should_auto_restart_after_exit(&self) -> bool {
        let state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.auto_mirror_enabled && !state.deliberate_stop
    }

    /// 记录一次自动重启失败并返回当前连续失败数。
    fn note_restart_failure(&self) -> u32 {
        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.consecutive_restart_failures = state.consecutive_restart_failures.saturating_add(1);
        state.consecutive_restart_failures
    }

    fn reset_restart_failures(&self) {
        self.state
            .lock()
            .expect("mirror state lock should not be poisoned")
            .consecutive_restart_failures = 0;
    }

    /// 连续重启超限：关闭自动投屏并标记主动停止，停止恢复循环
    /// （对应 C# 超限后停止自动恢复的语义）。
    fn abandon_auto_restart(&self) {
        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.auto_mirror_enabled = false;
        state.deliberate_stop = true;
    }

    fn start(&self, plan: CommandSpec) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        if state
            .child
            .as_mut()
            .is_some_and(|child| child_status_is_running(child.try_wait()))
        {
            return Err("ADB 投屏已在运行。".to_string());
        }
        state.child = None;
        // 上次 stop 终止失败的孤儿实例：启动前按 PID 强杀，避免双实例。
        for pid in state.stale_pids.drain(..) {
            kill_stale_mirror_process(pid);
        }
        let mut command = Command::new(plan.program);
        command.args(plan.args).envs(plan.environment);
        // scrcpy output is not a trace sink yet; never inherit raw child output
        // into the desktop console while the sealed observer adapter is pending.
        command.stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(directory) = plan.working_directory {
            command.current_dir(directory);
        }
        state.child = Some(
            command
                .spawn()
                .map_err(|error| format!("启动 ADB 投屏失败：{error}"))?,
        );
        Ok(())
    }

    /// 停止投屏并终止 scrcpy 进程树（pub(crate)：应用退出清理也走这条路径）。
    pub(crate) fn stop(&self) {
        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.deliberate_stop = true;
        // 手动停止结束当前投屏会话：复位失败计数，对齐 C# StopAsync。
        state.consecutive_restart_failures = 0;
        if let Some(mut child) = state.child.take() {
            let pid = child.id();
            if !terminate_process_tree(&mut child) {
                // 终止失败时句柄已被 take：记录 PID，下次 start 前强杀（N16），
                // 不能让孤儿 scrcpy 存活的同时同一路径被再次拉起成双实例。
                state.stale_pids.push(pid);
            }
        }
        self.stop_notify.notify_waiters();
    }
}

fn child_status_is_running(status: std::io::Result<Option<std::process::ExitStatus>>) -> bool {
    matches!(status, Ok(None) | Err(_))
}

pub fn build_start_plan(
    device_runtime: &DeviceRuntime,
    scrcpy_path: &Path,
    adb_path: &Path,
) -> Result<CommandSpec, String> {
    let serial = device_runtime.active_adb_serial()?;
    MirrorService::new(scrcpy_path, adb_path)
        .build_start_command(&serial, true)
        .map_err(|error| error.to_string())
}

async fn installed_paths(cancellation: &CancellationToken) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    // scrcpy 必须来自安装包，运行中不联网下载或执行 PATH 中的版本。
    let provisioner = crate::commands::resources::scrcpy_provisioner_with_downloader(
        nwflash_windows::bundled_resource_root(),
    );
    let _ = cancellation;
    let scrcpy = provisioner
        .installed_executable()
        .ok_or_else(|| "内置 scrcpy 缺失或校验失败，请重新安装应用。".to_string())?;
    Ok((
        scrcpy,
        std::path::PathBuf::from(nwflash_windows::bundled_platform_tool("adb.exe")),
    ))
}

pub async fn start_plan<P, Fut>(
    runtime: MirrorRuntime,
    coordinator: OperationCoordinator,
    // 供给工厂在操作体内以真实取消凭据调用（审计 A61）：future 形态
    // 在准入外构造，拿不到操作体的 cancellation——failover 下载需要
    // 它才能被 operation_cancel 终止。token 按值传入（克隆成本低），
    // 工厂返回的 future 不借用外部生命周期。
    plan_provision: P,
) -> Result<(), String>
where
    P: FnOnce(CancellationToken) -> Fut + Send + 'static,
    Fut: Future<Output = Result<CommandSpec, String>> + Send,
{
    let (started_tx, started_rx) = oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let started_tx_for_operation = started_tx.clone();
    let runtime_for_operation = runtime.clone();
    let coordinator_for_operation = coordinator.clone();

    tokio::spawn(async move {
        let result = coordinator
            .run_shared_async(
                OperationKind::Mirroring,
                "启动 ADB 投屏",
                move |context, cancellation| {
                    let started_tx = started_tx_for_operation.clone();
                    let runtime = runtime_for_operation.clone();
                    let coordinator = coordinator_for_operation.clone();
                    async move {
                        context.report_stage("启动 ADB 投屏");
                        // scrcpy 组件供给(failover 下载)在 Mirroring 准入之后
                        // 进行,与其他资源安装命令的门内语义一致(审查 B1)。
                        // 下载使用操作体的取消凭据(审计 A61)。
                        let plan = match plan_provision(cancellation.clone()).await {
                            Ok(plan) => plan,
                            Err(error) => {
                                if let Some(sender) = started_tx
                                    .lock()
                                    .expect("mirror start signal lock")
                                    .take()
                                {
                                    // 供给失败的具体原因直接透传(审计 B32):
                                    // 换成通用“内部错误”文案会让“已在运行”
                                    // 一类可自解问题无法排障。
                                    let _ = sender.send(Err(error.clone()));
                                }
                                return Err(DomainError::ExternalTool(error));
                            }
                        };
                        if let Err(error) = runtime.start(plan.clone()) {
                            if let Some(sender) =
                                started_tx.lock().expect("mirror start signal lock").take()
                            {
                                // 启动失败的具体原因直接透传(审计 B32)。
                                let _ = sender.send(Err(error.clone()));
                            }
                            return Err(DomainError::ExternalTool(error));
                        }
                        if let Some(sender) =
                            started_tx.lock().expect("mirror start signal lock").take()
                        {
                            let _ = sender.send(Ok(()));
                        }
                        context.report_progress(1.0);
                        loop {
                            if cancellation.is_cancelled()
                                || coordinator.admission_state()
                                    != nwflash_application::OperationAdmissionState::Running
                            {
                                runtime.stop();
                                break;
                            }
                            if !runtime.status().is_mirroring {
                                // scrcpy 自然退出：用户主动停止时到此为止；自动投屏
                                // 开启时的异常退出按 C# 语义延迟 1 秒自动拉起，
                                // 连续失败超过上限则停止恢复，防止崩溃循环。
                                if !runtime.should_auto_restart_after_exit() {
                                    break;
                                }
                                if runtime.note_restart_failure() >= MAX_CONSECUTIVE_MIRROR_RESTARTS
                                {
                                    runtime.abandon_auto_restart();
                                    break;
                                }
                                sleep(MIRROR_RESTART_DELAY).await;
                                if cancellation.is_cancelled() {
                                    break;
                                }
                                if let Err(error) = runtime.start(plan.clone()) {
                                    // “已在运行”不是 scrcpy 失败：新 start 与旧
                                    // 循环 1s 重启延迟竞态时会撞上，计入只会
                                    // 虚增计数加速 abandon（审计 B35）。
                                    if error.contains("已在运行") {
                                        continue;
                                    }
                                    if runtime.note_restart_failure()
                                        >= MAX_CONSECUTIVE_MIRROR_RESTARTS
                                    {
                                        runtime.abandon_auto_restart();
                                        return Err(DomainError::ExternalTool(error));
                                    }
                                    continue;
                                }
                                // 成功拉起后重置连续失败计数。
                                runtime.reset_restart_failures();
                                continue;
                            }
                            tokio::select! {
                                _ = sleep(Duration::from_millis(25)) => {}
                                _ = self_stop_notified(&runtime) => {}
                            }
                        }
                        Ok(())
                    }
                },
            )
            .await;

        if let Err(error) = result {
            if let Some(sender) = started_tx.lock().expect("mirror start signal lock").take() {
                let _ = sender.send(Err(result_to_domain_error(error).to_string()));
            }
        }
    });

    started_rx
        .await
        .map_err(|_| "ADB 投屏启动状态未知。".to_string())?
}

async fn self_stop_notified(runtime: &MirrorRuntime) {
    runtime.stop_notify.notified().await;
}

pub async fn reconcile_after_device_update(
    mirror_runtime: &MirrorRuntime,
    device_runtime: &DeviceRuntime,
    coordinator: &OperationCoordinator,
) -> Result<(), String> {
    if !mirror_runtime.should_auto_start() {
        return Ok(());
    }
    // A device must be present before attempting to start the mirror session.
    if device_runtime.active_adb_serial().is_err() {
        return Ok(());
    }
    start_plan_with_device_runtime(
        mirror_runtime.clone(),
        coordinator.clone(),
        device_runtime.clone(),
    )
    .await
}

/// 以设备运行时为准的启动计划供给：scrcpy 组件补齐与计划构建都在
/// `start_plan` 的 Mirroring 准入之后发生（供给工厂在门内闭包里以
/// 操作体取消凭据调用，failover 下载可被 operation_cancel 终止）。
async fn provisioned_plan(
    device_runtime: DeviceRuntime,
    cancellation: CancellationToken,
) -> Result<CommandSpec, String> {
    let (scrcpy, adb) = installed_paths(&cancellation).await?;
    build_start_plan(&device_runtime, &scrcpy, &adb)
}

async fn start_plan_with_device_runtime(
    mirror_runtime: MirrorRuntime,
    coordinator: OperationCoordinator,
    device_runtime: DeviceRuntime,
) -> Result<(), String> {
    start_plan(
        mirror_runtime,
        coordinator,
        move |cancellation| {
            provisioned_plan(device_runtime.clone(), cancellation)
        },
    )
    .await
}

#[tauri::command]
pub fn mirror_status(state: State<'_, AppState>) -> MirrorStatusDto {
    state.mirror_runtime.status()
}

#[tauri::command]
pub async fn mirror_start(state: State<'_, AppState>) -> Result<MirrorStatusDto, String> {
    state.mirror_runtime.begin_manual_start();
    start_plan_with_device_runtime(
        state.mirror_runtime.clone(),
        state.operation_coordinator.clone(),
        state.device_runtime.clone(),
    )
    .await?;
    Ok(state.mirror_runtime.status())
}

#[tauri::command]
pub async fn mirror_stop(state: State<'_, AppState>) -> Result<MirrorStatusDto, String> {
    let was_mirroring = state.mirror_runtime.status().is_mirroring;
    state.mirror_runtime.stop();
    if was_mirroring {
        let idle = state.operation_coordinator.wait_until_idle().await;
        drop(idle);
    }
    Ok(state.mirror_runtime.status())
}

#[tauri::command]
pub async fn mirror_set_auto(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<MirrorStatusDto, String> {
    state.mirror_runtime.set_auto_enabled(enabled);
    if enabled {
        reconcile_after_device_update(
            &state.mirror_runtime,
            &state.device_runtime,
            &state.operation_coordinator,
        )
        .await?;
    }
    Ok(state.mirror_runtime.status())
}

/// 终止 scrcpy 进程树；返回是否确认终止成功（kill + wait 收尾成功即视为成功）。
fn terminate_process_tree(child: &mut Child) -> bool {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new(r"C:\Windows\System32\taskkill.exe")
            .args(["/F", "/T", "/PID", pid.as_str()])
            .status();
    }
    let killed = child.kill().is_ok();
    let reaped = child.wait().is_ok();
    killed && reaped
}

/// 按 PID 强杀上次终止失败的孤儿 scrcpy（尽力而为，失败仅忽略）。
fn kill_stale_mirror_process(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new(r"C:\Windows\System32\taskkill.exe")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::commands::device::DeviceRuntime;
    use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot, DomainError};
    use tokio::time::{sleep, timeout, Duration};

    use super::*;

    #[test]
    fn start_plan_uses_the_confirmed_adb_serial_and_platform_adb_environment() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-tauri-mirror-{nonce}"));
        let scrcpy = root.join("scrcpy.exe");
        let adb = root.join("platform-tools").join("adb.exe");
        fs::create_dir_all(adb.parent().expect("ADB parent should exist"))
            .expect("ADB directory should be created");
        fs::write(&scrcpy, b"scrcpy").expect("scrcpy fixture should be written");
        fs::write(&adb, b"adb").expect("ADB fixture should be written");

        let runtime = DeviceRuntime::new();
        runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::AdbConnected,
                serial: "RF8T123".to_string(),
                connection_label: "ADB 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );

        let plan = build_start_plan(&runtime, &scrcpy, &adb)
            .expect("connected device and installed tools should create a plan");

        assert_eq!(plan.args, vec!["--serial", "RF8T123", "--stay-awake"]);
        assert_eq!(
            plan.environment,
            vec![("ADB".to_string(), adb.to_string_lossy().into_owned())]
        );
        assert!(!plan.args.iter().any(|argument| argument == "--adb-path"));
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn mirror_status_serializes_with_the_frontend_field_names() {
        let value = serde_json::to_value(MirrorStatusDto {
            is_mirroring: true,
            auto_mirror_enabled: false,
        })
        .expect("mirror status should serialize");

        assert_eq!(value["is_mirroring"], true);
        assert_eq!(value["auto_mirror_enabled"], false);
        assert!(value.get("isMirroring").is_none());
    }

    #[test]
    fn disabling_auto_mirror_latches_a_deliberate_stop() {
        let runtime = MirrorRuntime::new();
        runtime.set_auto_enabled(true);
        runtime.set_auto_enabled(false);

        let state = runtime.state.lock().expect("mirror state lock");
        assert!(!state.auto_mirror_enabled);
        assert!(state.deliberate_stop);
    }

    #[test]
    fn missing_scrcpy_requires_component_installation() {
        // scrcpy 缺失时组件按需补齐（ensure_installed 走 failover 下载器），
        // 这里只校验错误文案中的安装指引仍保留。
        let error = "未检测到内置 scrcpy.exe，请重新安装应用。";

        assert!(error.contains("重新安装应用"));
        assert!(error.contains("scrcpy"));
    }

    #[test]
    fn unknown_child_status_blocks_a_concurrent_start_fail_closed() {
        use std::io;

        assert!(child_status_is_running(Ok(None)));
        assert!(child_status_is_running(Err(io::Error::other(
            "status unavailable"
        ))));
    }

    /// 把已构建的启动计划包装成立即就绪的供给工厂（测试用：不触网络
    /// 供给链；工厂形态对齐生产——供给发生在门内并接收取消凭据）。
    fn ready_plan(
        plan: CommandSpec,
    ) -> impl FnOnce(
        CancellationToken,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<CommandSpec, String>> + Send>>
    + Send
    + 'static {
        move |_| Box::pin(async move { Ok(plan) })
    }

    fn long_running_plan() -> CommandSpec {        #[cfg(windows)]
        {
            CommandSpec {
                program: "cmd.exe".to_string(),
                args: vec![
                    "/C".to_string(),
                    "ping".to_string(),
                    "127.0.0.1".to_string(),
                    "-n".to_string(),
                    "30".to_string(),
                ],
                working_directory: None,
                environment: Vec::new(),
            }
        }

        #[cfg(not(windows))]
        {
            CommandSpec {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "sleep 30".to_string()],
                working_directory: None,
                environment: Vec::new(),
            }
        }
    }

    async fn wait_for_mirror(runtime: &MirrorRuntime) {
        timeout(Duration::from_secs(3), async {
            loop {
                if runtime.status().is_mirroring {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("local mirror child should become visible");
    }

    #[tokio::test]
    async fn active_mirror_shares_admission_without_blocking_device_operations() {
        let runtime = MirrorRuntime::new();
        let coordinator = OperationCoordinator::default();
        let running = tokio::spawn(start_plan(
            runtime.clone(),
            coordinator.clone(),
            ready_plan(long_running_plan()),
        ));
        wait_for_mirror(&runtime).await;

        // 投屏走共享通道：设备独占操作（刷写）不再被它拦截，也不返回 InProgress。
        let flashing = coordinator
            .run_async(OperationKind::Flashing, "concurrent flash", |_, _| async {
                Ok::<(), DomainError>(())
            })
            .await;
        assert!(
            flashing.is_ok(),
            "mirroring must not block device-exclusive operations: {flashing:?}"
        );

        // 但“空闲”依然要求共享通道清空：投屏未停时不得拿到 idle 租约。
        assert!(
            coordinator.try_acquire_idle().is_err(),
            "an active mirror session must keep the coordinator non-idle"
        );

        runtime.stop();
        let start_result = running.await.expect("mirror task should join");

        assert!(start_result.is_ok());
        assert!(!runtime.status().is_mirroring);
        let idle = timeout(Duration::from_secs(3), coordinator.wait_until_idle())
            .await
            .expect("mirror stop should release the coordinator permit");
        drop(idle);
    }

    #[tokio::test]
    async fn exit_pending_stops_child_before_terminating_can_acquire_idle() {
        let runtime = MirrorRuntime::new();
        let coordinator = OperationCoordinator::default();
        let running = tokio::spawn(start_plan(
            runtime.clone(),
            coordinator.clone(),
            ready_plan(long_running_plan()),
        ));
        wait_for_mirror(&runtime).await;

        assert_eq!(
            coordinator.request_exit_pending(),
            nwflash_application::OperationAdmissionState::ExitPending
        );
        let idle = timeout(Duration::from_secs(3), coordinator.wait_until_idle())
            .await
            .expect("mirror child should release coordinator during exit pending");
        assert!(!runtime.status().is_mirroring);
        coordinator
            .begin_terminating(&idle)
            .expect("terminating should begin after mirror cleanup");

        runtime.stop();
        let start_result = running.await.expect("mirror task should join");
        assert!(start_result.is_ok());
    }

    #[tokio::test]
    async fn repeated_start_does_not_create_a_second_child() {
        let runtime = MirrorRuntime::new();
        let coordinator = OperationCoordinator::default();
        let first = tokio::spawn(start_plan(
            runtime.clone(),
            coordinator.clone(),
            ready_plan(long_running_plan()),
        ));
        wait_for_mirror(&runtime).await;

        let second = start_plan(runtime.clone(), coordinator, ready_plan(long_running_plan())).await;
        runtime.stop();
        let first_result = first.await.expect("first mirror task should join");

        assert!(first_result.is_ok());
        assert!(second.is_err());
        assert!(!runtime.status().is_mirroring);
    }
}
