use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use nwflash_application::{
    parse_remote_listing, result_to_domain_error, CommandSpec, FileManagerService,
};
use nwflash_domain::{DeviceFileEntry, DomainError, OperationKind};
use nwflash_windows::process::{run_command_with_cancel, ProcessCommand};
use tauri::State;
use tokio::task;

use crate::commands::device::DeviceRuntime;
use crate::AppState;

pub fn build_list_plan(
    device_runtime: &DeviceRuntime,
    remote_directory: &str,
) -> Result<CommandSpec, String> {
    let serial = device_runtime.active_adb_serial()?;
    FileManagerService::bundled()
        .build_list_command(&serial, remote_directory)
        .map_err(|error| error.to_string())
}

pub fn build_delete_plan(
    device_runtime: &DeviceRuntime,
    remote_path: &str,
) -> Result<CommandSpec, String> {
    let serial = device_runtime.active_adb_serial()?;
    FileManagerService::bundled()
        .build_delete_command(&serial, remote_path)
        .map_err(|error| error.to_string())
}

pub fn build_download_plan(
    device_runtime: &DeviceRuntime,
    remote_path: &str,
    destination_path: &Path,
) -> Result<CommandSpec, String> {
    let serial = device_runtime.active_adb_serial()?;
    FileManagerService::bundled()
        .build_pull_command(&serial, remote_path, destination_path)
        .map_err(|error| error.to_string())
}

pub fn build_upload_plan(
    device_runtime: &DeviceRuntime,
    source_path: &Path,
    remote_directory: &str,
) -> Result<CommandSpec, String> {
    let serial = device_runtime.active_adb_serial()?;
    FileManagerService::bundled()
        .build_push_command(&serial, source_path, remote_directory)
        .map_err(|error| error.to_string())
}

pub fn build_install_apk_plan(
    device_runtime: &DeviceRuntime,
    apk_path: &Path,
) -> Result<CommandSpec, String> {
    let serial = device_runtime.active_adb_serial()?;
    FileManagerService::bundled()
        .build_install_apk_command(&serial, apk_path)
        .map_err(|error| error.to_string())
}

async fn execute_file_command(
    state: &AppState,
    command: CommandSpec,
    operation_kind: OperationKind,
    operation_title: &'static str,
) -> Result<(), String> {
    state
        .operation_coordinator
        .run_async(
            operation_kind,
            operation_title,
            move |context, cancellation| async move {
                context.report_stage(operation_title);
                let cancellation_for_command = cancellation.clone();
                let process_command = ProcessCommand {
                    program: command.program,
                    args: command.args,
                    working_directory: command.working_directory,
                    environment: command.environment,
                };
                let output = task::spawn_blocking(move || {
                    run_command_with_cancel(process_command, None, move || {
                        cancellation_for_command.is_cancelled()
                    })
                })
                .await
                .map_err(|error| {
                    DomainError::Internal(format!("{operation_title}调度失败：{error}"))
                })??;
                if output.exit_code != 0 {
                    return Err(DomainError::ExternalTool(format!(
                        "{operation_title}失败，退出码 {}：{}",
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

pub fn parse_list_result(directory: &str, stdout: &str) -> Vec<DeviceFileEntry> {
    parse_remote_listing(directory, stdout)
}

#[tauri::command]
pub async fn files_list(
    state: State<'_, AppState>,
    remote_directory: String,
) -> Result<Vec<DeviceFileEntry>, String> {
    let command = build_list_plan(&state.device_runtime, &remote_directory)?;
    let entries = Arc::new(Mutex::new(Vec::new()));
    let result_entries = entries.clone();
    let directory_for_result = remote_directory.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Discovering,
            "正在读取设备目录",
            move |context, cancellation| async move {
                context.report_stage("正在读取设备目录");
                let cancellation_for_command = cancellation.clone();
                let process_command = ProcessCommand {
                    program: command.program,
                    args: command.args,
                    working_directory: command.working_directory,
                    environment: command.environment,
                };
                let output = task::spawn_blocking(move || {
                    run_command_with_cancel(process_command, None, move || {
                        cancellation_for_command.is_cancelled()
                    })
                })
                .await
                .map_err(|error| DomainError::Internal(format!("目录读取调度失败：{error}")))??;
                if output.exit_code != 0 {
                    return Err(DomainError::ExternalTool(format!(
                        "读取设备目录失败，退出码 {}：{}",
                        output.exit_code, output.stderr
                    )));
                }
                *result_entries
                    .lock()
                    .expect("file list result lock should not be poisoned") =
                    parse_list_result(&directory_for_result, &output.stdout);
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let result = entries
        .lock()
        .expect("file list result lock should not be poisoned")
        .clone();
    Ok(result)
}

#[tauri::command]
pub async fn files_delete(state: State<'_, AppState>, remote_path: String) -> Result<(), String> {
    let command = build_delete_plan(&state.device_runtime, &remote_path)?;

    execute_file_command(&state, command, OperationKind::Transferring, "删除设备文件").await
}

#[tauri::command]
pub async fn files_download(
    state: State<'_, AppState>,
    remote_path: String,
    destination_path: String,
) -> Result<(), String> {
    let command = build_download_plan(
        &state.device_runtime,
        &remote_path,
        Path::new(&destination_path),
    )?;
    execute_file_command(&state, command, OperationKind::Transferring, "下载设备文件").await
}

#[tauri::command]
pub async fn files_upload(
    state: State<'_, AppState>,
    source_path: String,
    remote_directory: String,
) -> Result<(), String> {
    let command = build_upload_plan(
        &state.device_runtime,
        Path::new(&source_path),
        &remote_directory,
    )?;
    execute_file_command(&state, command, OperationKind::Transferring, "上传设备文件").await
}

#[tauri::command]
pub async fn files_install_apk(state: State<'_, AppState>, apk_path: String) -> Result<(), String> {
    let command = build_install_apk_plan(&state.device_runtime, Path::new(&apk_path))?;
    execute_file_command(&state, command, OperationKind::Installing, "安装 APK").await
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::commands::device::DeviceRuntime;
    use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot};

    #[test]
    fn list_plan_uses_the_current_adb_device_serial_instead_of_a_frontend_serial() {
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

        let command = build_list_plan(&runtime, "/sdcard/Download")
            .expect("current ADB device should build a file list plan");

        assert_eq!(
            command.program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            command.args,
            vec!["-s", "RF8T123", "shell", "ls -laL -- '/sdcard/Download/'"]
        );
    }

    #[test]
    fn list_result_projects_only_parsed_file_entries() {
        let entries = parse_list_result(
            "/sdcard",
            "drwxrwx--x 2 u0_a123 media_rw 4096 2026-08-10 11:20 Download\ninvalid",
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Download");
        assert!(entries[0].is_directory);
    }

    #[test]
    fn delete_plan_uses_the_current_adb_device_serial_and_a_fixed_delete_template() {
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

        let command = build_delete_plan(&runtime, "/sdcard/Download/old file.zip")
            .expect("current ADB device should build a file delete plan");

        assert_eq!(
            command.program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            command.args,
            vec![
                "-s",
                "RF8T123",
                "shell",
                "rm -rf -- '/sdcard/Download/old file.zip'"
            ]
        );
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("nwflash-files-{label}-{nonce}"));
        fs::create_dir_all(&directory).expect("temporary directory should be created");
        directory
    }

    fn active_adb_runtime() -> DeviceRuntime {
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
        runtime
    }

    #[test]
    fn download_plan_uses_the_selected_exact_destination_without_a_frontend_serial() {
        let directory = temporary_directory("download");
        let destination = directory.join("chosen name.zip");

        let command = build_download_plan(
            &active_adb_runtime(),
            "/sdcard/Download/update.zip",
            &destination,
        )
        .expect("current ADB device should build a download plan");

        assert_eq!(
            command.args,
            vec![
                "-s",
                "RF8T123",
                "pull",
                "/sdcard/Download/update.zip",
                destination.to_string_lossy().as_ref(),
            ]
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removed");
    }

    #[test]
    fn upload_plan_uses_the_current_device_and_current_remote_directory() {
        let directory = temporary_directory("upload");
        let source = directory.join("update.zip");
        fs::write(&source, b"fixture").expect("source fixture should be written");

        let command = build_upload_plan(&active_adb_runtime(), &source, "/sdcard/Download")
            .expect("current ADB device should build an upload plan");

        assert_eq!(
            command.args,
            vec![
                "-s",
                "RF8T123",
                "push",
                source.to_string_lossy().as_ref(),
                "/sdcard/Download/update.zip",
            ]
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removed");
    }

    #[test]
    fn apk_install_plan_rejects_non_apk_files_before_adb_execution() {
        let directory = temporary_directory("apk");
        let source = directory.join("manager.zip");
        fs::write(&source, b"fixture").expect("source fixture should be written");

        let error = build_install_apk_plan(&active_adb_runtime(), &source)
            .expect_err("a non-APK selection must not produce an install plan");

        assert!(error.contains("只能安装 APK 文件"));
        fs::remove_dir_all(directory).expect("temporary directory should be removed");
    }
}
