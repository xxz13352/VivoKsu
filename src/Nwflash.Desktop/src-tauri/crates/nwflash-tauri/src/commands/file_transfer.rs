//! Tauri commands for adb-root file transfer command inspection and execution.

use std::path::Path;

use serde::Serialize;
use tauri::State;
use tokio::task;

use nwflash_application::{result_to_domain_error, CommandSpec, FileManagerService};
use nwflash_domain::OperationKind;
use nwflash_windows::process::{run_command_with_cancel, ProcessCommand};

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct ProcessCommandDto {
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: Option<String>,
    pub environment: Vec<(String, String)>,
}

impl From<CommandSpec> for ProcessCommandDto {
    fn from(command: CommandSpec) -> Self {
        Self {
            program: command.program,
            args: command.args,
            working_directory: command
                .working_directory
                .map(|value| value.to_string_lossy().into_owned()),
            environment: command.environment,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FileTransferCommandDto {
    pub command: ProcessCommandDto,
}

#[derive(Debug, Serialize)]
pub struct CommandExecutionResultDto {
    pub command_count: usize,
    pub executed_count: usize,
}

#[tauri::command]
pub fn file_transfer_build_pull_command(
    serial: String,
    device_path: String,
    local_path: String,
) -> Result<FileTransferCommandDto, String> {
    let service = FileManagerService::bundled();
    let command = service
        .build_pull_command(&serial, &device_path, Path::new(&local_path))
        .map_err(|error| error.to_string())?;
    Ok(FileTransferCommandDto {
        command: command.into(),
    })
}

#[tauri::command]
pub async fn file_transfer_run_pull_command(
    state: State<'_, AppState>,
    serial: String,
    device_path: String,
    local_path: String,
) -> Result<CommandExecutionResultDto, String> {
    let service = FileManagerService::bundled();
    let command = service
        .build_pull_command(&serial, &device_path, Path::new(&local_path))
        .map_err(|error| error.to_string())?;

    state
        .operation_coordinator
        .run_async(
            OperationKind::Transferring,
            "下载设备文件".to_string(),
            move |context, cancellation| async move {
                let cancellation_for_command = cancellation.clone();

                if cancellation.is_cancelled() {
                    return Err(nwflash_domain::DomainError::UserCancelled(
                        "运行被用户取消".to_string(),
                    ));
                }

                context.report_stage("下载设备文件");
                let process_command = ProcessCommand {
                    program: command.program.clone(),
                    args: command.args.clone(),
                    working_directory: command.working_directory.clone(),
                    environment: command.environment.clone(),
                };

                let output = task::spawn_blocking(move || {
                    run_command_with_cancel(process_command, None, move || {
                        cancellation_for_command.is_cancelled()
                    })
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("命令执行调度失败：{error}"))
                })?
                .map_err(|error| error)?;

                if output.exit_code != 0 {
                    return Err(nwflash_domain::DomainError::ExternalTool(format!(
                        "下载设备文件失败，退出码 {}：{}",
                        output.exit_code, output.stderr
                    )));
                }

                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    Ok(CommandExecutionResultDto {
        command_count: 1,
        executed_count: 1,
    })
}

#[tauri::command]
pub fn file_transfer_build_push_command(
    serial: String,
    local_path: String,
    device_path: String,
) -> Result<FileTransferCommandDto, String> {
    let service = FileManagerService::bundled();
    let command = service
        .build_push_command(&serial, Path::new(&local_path), &device_path)
        .map_err(|error| error.to_string())?;
    Ok(FileTransferCommandDto {
        command: command.into(),
    })
}

#[tauri::command]
pub async fn file_transfer_run_push_command(
    state: State<'_, AppState>,
    serial: String,
    local_path: String,
    device_path: String,
) -> Result<CommandExecutionResultDto, String> {
    let service = FileManagerService::bundled();
    let command = service
        .build_push_command(&serial, Path::new(&local_path), &device_path)
        .map_err(|error| error.to_string())?;

    state
        .operation_coordinator
        .run_async(
            OperationKind::Transferring,
            "上传设备文件".to_string(),
            move |context, cancellation| async move {
                let cancellation_for_command = cancellation.clone();

                if cancellation.is_cancelled() {
                    return Err(nwflash_domain::DomainError::UserCancelled(
                        "运行被用户取消".to_string(),
                    ));
                }

                context.report_stage("上传设备文件");
                let process_command = ProcessCommand {
                    program: command.program.clone(),
                    args: command.args.clone(),
                    working_directory: command.working_directory.clone(),
                    environment: command.environment.clone(),
                };

                let output = task::spawn_blocking(move || {
                    run_command_with_cancel(process_command, None, move || {
                        cancellation_for_command.is_cancelled()
                    })
                })
                .await
                .map_err(|error| {
                    nwflash_domain::DomainError::Internal(format!("命令执行调度失败：{error}"))
                })?
                .map_err(|error| error)?;

                if output.exit_code != 0 {
                    return Err(nwflash_domain::DomainError::ExternalTool(format!(
                        "上传设备文件失败，退出码 {}：{}",
                        output.exit_code, output.stderr
                    )));
                }

                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    Ok(CommandExecutionResultDto {
        command_count: 1,
        executed_count: 1,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-files-command-{label}-{nonce}"));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    #[test]
    fn build_pull_command_returns_an_adb_pull_without_shell_text() {
        let root = temporary_directory("pull");
        let target = root.join("selected-update.zip");
        let response = file_transfer_build_pull_command(
            "SN-002".to_string(),
            "/sdcard/Download/update.zip".to_string(),
            target.to_string_lossy().into_owned(),
        )
        .expect("pull command dto should build");

        assert_eq!(response.command.program, "adb.exe");
        assert_eq!(
            response.command.args,
            vec![
                "-s",
                "SN-002",
                "pull",
                "/sdcard/Download/update.zip",
                target.to_string_lossy().as_ref(),
            ]
        );
        assert!(!response
            .command
            .args
            .iter()
            .any(|argument| argument == "shell"));
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn build_pull_command_rejects_remote_path_traversal() {
        let root = temporary_directory("traversal");
        let result = file_transfer_build_pull_command(
            "SN-002".to_string(),
            "/sdcard/Download/../private.bin".to_string(),
            root.join("out.bin").to_string_lossy().into_owned(),
        );

        let err = result.expect_err("bad path should fail");
        assert!(err.contains("设备路径"));
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn build_push_command_rejects_empty_serial_without_invoking_adb() {
        let root = temporary_directory("push");
        let source = root.join("update.zip");
        fs::write(&source, b"fixture").expect("source fixture should be written");
        let result = file_transfer_build_push_command(
            String::new(),
            source.to_string_lossy().into_owned(),
            "/sdcard/Download".to_string(),
        );

        let err = result.expect_err("empty serial should fail");
        assert!(err.contains("设备串口不能为空"));
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }
}
