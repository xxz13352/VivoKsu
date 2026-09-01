use std::{
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

use nwflash_application::{
    result_to_domain_error, CommandSpec, MirrorService, OperationCoordinator,
};
use nwflash_domain::{DomainError, OperationKind};
use nwflash_infrastructure::ScrcpyProvisioner;
use serde::Serialize;
use tauri::State;
use tokio::sync::{oneshot, Notify};
use tokio::time::{sleep, Duration};

use crate::{commands::device::DeviceRuntime, AppState};

const MIRROR_START_FAILED_MESSAGE: &str =
    "内部错误: 外部工具执行失败，请检查设备连接和所需组件后重试。";

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
        self.state
            .lock()
            .expect("mirror state lock should not be poisoned")
            .deliberate_stop = false;
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
    }

    fn should_auto_start(&self) -> bool {
        let state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.auto_mirror_enabled && !state.deliberate_stop && state.child.is_none()
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

    fn stop(&self) {
        let mut state = self
            .state
            .lock()
            .expect("mirror state lock should not be poisoned");
        state.deliberate_stop = true;
        if let Some(mut child) = state.child.take() {
            terminate_process_tree(&mut child);
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

async fn installed_paths() -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let provisioner = ScrcpyProvisioner::bundled(nwflash_windows::bundled_resource_root());
    let scrcpy = require_installed_scrcpy(provisioner.installed_executable())?;
    Ok((
        scrcpy,
        std::path::PathBuf::from(nwflash_windows::bundled_platform_tool("adb.exe")),
    ))
}

fn require_installed_scrcpy(
    path: Option<std::path::PathBuf>,
) -> Result<std::path::PathBuf, String> {
    path.ok_or_else(|| "未检测到内置 scrcpy.exe，请重新安装应用。".to_string())
}

pub async fn start_plan(
    runtime: MirrorRuntime,
    coordinator: OperationCoordinator,
    plan: CommandSpec,
) -> Result<(), String> {
    let (started_tx, started_rx) = oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let started_tx_for_operation = started_tx.clone();
    let runtime_for_operation = runtime.clone();
    let coordinator_for_operation = coordinator.clone();

    tokio::spawn(async move {
        let result = coordinator
            .run_async(
                OperationKind::Mirroring,
                "启动 ADB 投屏",
                move |context, cancellation| {
                    let started_tx = started_tx_for_operation.clone();
                    let runtime = runtime_for_operation.clone();
                    let coordinator = coordinator_for_operation.clone();
                    async move {
                        context.report_stage("启动 ADB 投屏");
                        if let Err(error) = runtime.start(plan) {
                            if let Some(sender) =
                                started_tx.lock().expect("mirror start signal lock").take()
                            {
                                let _ = sender.send(Err(MIRROR_START_FAILED_MESSAGE.to_string()));
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
                                break;
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
    let (scrcpy, adb) = installed_paths().await?;
    let plan = build_start_plan(device_runtime, &scrcpy, &adb)?;
    start_plan(mirror_runtime.clone(), coordinator.clone(), plan).await
}

#[tauri::command]
pub fn mirror_status(state: State<'_, AppState>) -> MirrorStatusDto {
    state.mirror_runtime.status()
}

#[tauri::command]
pub async fn mirror_start(state: State<'_, AppState>) -> Result<MirrorStatusDto, String> {
    state.mirror_runtime.begin_manual_start();
    let (scrcpy, adb) = installed_paths().await?;
    let plan = build_start_plan(&state.device_runtime, &scrcpy, &adb)?;
    start_plan(
        state.mirror_runtime.clone(),
        state.operation_coordinator.clone(),
        plan,
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

fn terminate_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new(r"C:\Windows\System32\taskkill.exe")
            .args(["/F", "/T", "/PID", pid.as_str()])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
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
        let error = require_installed_scrcpy(None).expect_err("missing scrcpy should be reported");

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

    fn long_running_plan() -> CommandSpec {
        #[cfg(windows)]
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
    async fn long_lived_mirror_keeps_admission_until_child_stops() {
        let runtime = MirrorRuntime::new();
        let coordinator = OperationCoordinator::default();
        let running = tokio::spawn(start_plan(
            runtime.clone(),
            coordinator.clone(),
            long_running_plan(),
        ));
        wait_for_mirror(&runtime).await;

        let flashing = coordinator
            .run_async(OperationKind::Flashing, "concurrent flash", |_, _| async {
                Ok::<(), DomainError>(())
            })
            .await;
        runtime.stop();
        let start_result = running.await.expect("mirror task should join");

        assert!(start_result.is_ok());
        assert!(matches!(
            flashing,
            Err(nwflash_application::OperationCoordinatorError::InProgress)
        ));
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
            long_running_plan(),
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
            long_running_plan(),
        ));
        wait_for_mirror(&runtime).await;

        let second = start_plan(runtime.clone(), coordinator, long_running_plan()).await;
        runtime.stop();
        let first_result = first.await.expect("first mirror task should join");

        assert!(first_result.is_ok());
        assert!(second.is_err());
        assert!(!runtime.status().is_mirroring);
    }
}
