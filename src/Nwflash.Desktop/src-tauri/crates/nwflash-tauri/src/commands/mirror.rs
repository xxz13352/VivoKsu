use std::{
    path::Path,
    process::{Child, Command},
    sync::{Arc, Mutex},
};

use nwflash_application::{
    result_to_domain_error, CommandSpec, MirrorService, OperationCoordinator,
};
use nwflash_domain::{DomainError, OperationKind};
use nwflash_infrastructure::ScrcpyProvisioner;
use serde::Serialize;
use tauri::State;

use crate::{commands::device::DeviceRuntime, AppState};

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
}

impl MirrorRuntime {
    pub fn new() -> Self {
        Self::default()
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
            .is_some_and(|child| child.try_wait().ok().flatten().is_none())
        {
            return Err("ADB 投屏已在运行。".to_string());
        }
        state.child = None;
        let mut command = Command::new(plan.program);
        command.args(plan.args).envs(plan.environment);
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
    }
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
    coordinator
        .run_async(
            OperationKind::Mirroring,
            "启动 ADB 投屏",
            move |context, _| async move {
                context.report_stage("启动 ADB 投屏");
                runtime.start(plan).map_err(DomainError::ExternalTool)?;
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())
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
    let runtime = state.mirror_runtime.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Mirroring,
            "停止 ADB 投屏",
            move |context, _| async move {
                context.report_stage("停止 ADB 投屏");
                runtime.stop();
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;
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
        let _ = Command::new("taskkill")
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
    use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot};

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
}
