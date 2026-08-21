use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use nwflash_application::{
    parse_adb_root_partition_table, parse_fastboot_partition_table, result_to_domain_error,
    PartitionSelectionSummary, PartitionWorkspace,
};
use nwflash_domain::{
    DomainError, FlashImageInfo, OperationKind, PartitionExecutionPlan, PartitionSnapshot,
    PartitionTransportKind,
};
use nwflash_windows::{
    process::{run_command_with_cancel, run_command_with_file_stdout_and_cancel, ProcessCommand},
    DeviceTransport, PlatformTools,
};
use serde::Serialize;
use tauri::State;
use tokio::task;

use crate::{commands::device::DeviceRuntime, AppState};

const ADB_ROOT_DISCOVER_COMMAND: &str = "for d in /dev/block/by-name /dev/block/bootdevice/by-name /dev/block/platform/*/by-name; do [ -d \"$d\" ] || continue; for p in \"$d\"/*; do [ -e \"$p\" ] || continue; n=${p##*/}; r=$(readlink -f \"$p\") || continue; s=$(blockdev --getsize64 \"$r\" 2>/dev/null); m=0; grep -Fq \" $r \" /proc/mounts && m=1; printf '%s|%s|%s|%s\\n' \"$n\" \"$r\" \"$s\" \"$m\"; done; done";
const ADB_ROOT_RESOLVE_BY_NAME_TEMPLATE: &str = "for d in /dev/block/by-name /dev/block/bootdevice/by-name /dev/block/platform/*/by-name; do [ -e \"$d/{partition_name}\" ] || continue; readlink -f \"$d/{partition_name}\"; break; done";

#[derive(Clone, Default)]
pub struct PartitionWorkspaceRuntime {
    workspace: Arc<Mutex<PartitionWorkspace>>,
}

#[derive(Debug, Serialize)]
pub struct PartitionEraseConfirmationDto {
    pub task_count: usize,
    pub high_risk_count: usize,
    pub mounted_count: usize,
}

#[derive(Debug, Serialize)]
pub struct PartitionImageMappingDto {
    pub mapped_count: usize,
}

pub fn confirmation_from_summary(
    summary: PartitionSelectionSummary,
) -> PartitionEraseConfirmationDto {
    PartitionEraseConfirmationDto {
        task_count: summary.task_count,
        high_risk_count: summary.high_risk_count,
        mounted_count: summary.mounted_count,
    }
}

impl PartitionWorkspaceRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_snapshot(&self, snapshot: PartitionSnapshot) {
        self.workspace
            .lock()
            .expect("partition workspace lock should not be poisoned")
            .apply_snapshot(snapshot);
    }

    pub fn cached_snapshot(&self) -> Option<PartitionSnapshot> {
        self.workspace
            .lock()
            .expect("partition workspace lock should not be poisoned")
            .cached_snapshot()
    }

    pub fn build_erase_plan(
        &self,
        selected_names: &[String],
    ) -> Result<PartitionExecutionPlan, String> {
        self.workspace
            .lock()
            .expect("partition workspace lock should not be poisoned")
            .build_erase_plan(selected_names)
            .map_err(|error| error.to_string())
    }

    pub fn selection_summary(
        &self,
        selected_names: &[String],
    ) -> Result<PartitionSelectionSummary, String> {
        self.workspace
            .lock()
            .expect("partition workspace lock should not be poisoned")
            .selection_summary(selected_names)
            .map_err(|error| error.to_string())
    }

    pub fn map_images(&self, images: &[FlashImageInfo]) -> Vec<String> {
        self.workspace
            .lock()
            .expect("partition workspace lock should not be poisoned")
            .map_images(images)
    }

    pub fn build_write_plan(
        &self,
        selected_names: &[String],
    ) -> Result<PartitionExecutionPlan, String> {
        self.workspace
            .lock()
            .expect("partition workspace lock should not be poisoned")
            .build_write_plan(selected_names)
            .map_err(|error| error.to_string())
    }

    pub fn build_backup_plan(
        &self,
        selected_names: &[String],
        output_directory: &str,
    ) -> Result<PartitionExecutionPlan, String> {
        self.workspace
            .lock()
            .expect("partition workspace lock should not be poisoned")
            .build_backup_plan(selected_names, output_directory)
            .map_err(|error| error.to_string())
    }
}

pub fn build_partition_refresh_plan(
    device_runtime: &DeviceRuntime,
) -> Result<ProcessCommand, String> {
    let serial = device_runtime.active_fastboot_serial()?;
    DeviceTransport::new(PlatformTools::bundled())
        .build_fastboot_getvar_command(&serial, "all")
        .map_err(|error| error.to_string())
}

pub fn build_adb_root_discovery_commands(
    device_runtime: &DeviceRuntime,
) -> Result<Vec<ProcessCommand>, String> {
    let serial = device_runtime.active_adb_serial()?;
    let transport = DeviceTransport::new(PlatformTools::bundled());
    Ok(vec![
        transport
            .build_adb_root_shell_command(&serial, "id -u")
            .map_err(|error| error.to_string())?,
        transport
            .build_adb_root_shell_command(&serial, "getprop ro.boot.slot_suffix")
            .map_err(|error| error.to_string())?,
        transport
            .build_adb_root_shell_command(&serial, ADB_ROOT_DISCOVER_COMMAND)
            .map_err(|error| error.to_string())?,
    ])
}

pub fn build_adb_root_path_resolution_command(
    device_runtime: &DeviceRuntime,
    partition_name: &str,
) -> Result<ProcessCommand, String> {
    if partition_name.is_empty()
        || !partition_name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        return Err("无效的分区名，已阻止执行。".to_string());
    }
    let serial = device_runtime.active_adb_serial()?;

    DeviceTransport::new(PlatformTools::bundled())
        .build_adb_root_shell_command(
            &serial,
            &ADB_ROOT_RESOLVE_BY_NAME_TEMPLATE.replace("{partition_name}", partition_name),
        )
        .map_err(|error| error.to_string())
}

pub fn resolve_partition_refresh_transport(
    device_runtime: &DeviceRuntime,
    requested: PartitionTransportKind,
) -> Result<PartitionTransportKind, String> {
    match requested {
        PartitionTransportKind::Fastboot => {
            device_runtime.active_fastboot_serial()?;
            Ok(PartitionTransportKind::Fastboot)
        }
        PartitionTransportKind::AdbRoot => {
            device_runtime.active_adb_serial()?;
            Ok(PartitionTransportKind::AdbRoot)
        }
        PartitionTransportKind::Automatic => {
            if device_runtime.active_fastboot_serial().is_ok() {
                Ok(PartitionTransportKind::Fastboot)
            } else {
                device_runtime.active_adb_serial()?;
                Ok(PartitionTransportKind::AdbRoot)
            }
        }
    }
}

fn partition_refresh_operation_title(is_fastboot: bool) -> &'static str {
    if is_fastboot {
        "读取 Fastboot 分区表"
    } else {
        "读取 ADB Root 分区表"
    }
}

fn finalize_backup_file(
    partial_path: &Path,
    output_path: &Path,
    expected_size: Option<i64>,
) -> Result<(), String> {
    let result = (|| {
        let actual_size = std::fs::metadata(partial_path)
            .map_err(|error| format!("无法读取临时备份文件：{error}"))?
            .len();
        if let Some(expected_size) = expected_size {
            if i64::try_from(actual_size).unwrap_or(i64::MAX) != expected_size {
                return Err(format!(
                    "备份文件大小不符（期望 {expected_size} 字节，实际 {actual_size} 字节），已放弃本次备份。"
                ));
            }
        } else if actual_size == 0 {
            return Err("备份文件为空，已放弃本次备份。".to_string());
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(partial_path);
        return result;
    }

    replace_backup_file_atomically(partial_path, output_path)
}

fn discard_backup_partial(partial_path: &Path) {
    let _ = std::fs::remove_file(partial_path);
}

#[cfg(windows)]
fn replace_backup_file_atomically(partial_path: &Path, output_path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let existing: Vec<u16> = partial_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replacement: Vec<u16> = output_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Windows replaces the destination only after the source has been accepted.
    let moved = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            replacement.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(format!(
            "无法原子替换备份文件：{}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_backup_file_atomically(partial_path: &Path, output_path: &Path) -> Result<(), String> {
    std::fs::rename(partial_path, output_path).map_err(|error| format!("无法完成备份文件：{error}"))
}

fn build_adb_root_backup_command(
    serial: &str,
    task: &nwflash_domain::PartitionTask,
) -> Result<(ProcessCommand, PathBuf), String> {
    let output_path = task
        .output_path
        .as_ref()
        .ok_or_else(|| "分区备份输出路径缺失。".to_string())?;
    let partial_path = PathBuf::from(format!("{output_path}.partial"));
    let partial_text = partial_path
        .to_str()
        .ok_or_else(|| "分区备份输出路径包含不支持的字符。".to_string())?;
    let transport = DeviceTransport::new(PlatformTools::bundled());
    let command = transport
        .build_adb_root_copy_from_device_command(serial, &task.device_path, partial_text)
        .map_err(|error| error.to_string())?;
    Ok((command, partial_path))
}

#[tauri::command]
pub fn partitions_cached_snapshot(state: State<'_, AppState>) -> Option<PartitionSnapshot> {
    state.partition_workspace.cached_snapshot()
}

#[tauri::command]
pub async fn partitions_refresh(
    state: State<'_, AppState>,
    requested_transport: Option<PartitionTransportKind>,
) -> Result<PartitionSnapshot, String> {
    let transport = resolve_partition_refresh_transport(
        &state.device_runtime,
        requested_transport.unwrap_or(PartitionTransportKind::Automatic),
    )?;
    let is_fastboot = matches!(transport, PartitionTransportKind::Fastboot);
    let fastboot_command = if is_fastboot {
        Some(build_partition_refresh_plan(&state.device_runtime)?)
    } else {
        None
    };
    let adb_root_commands = if is_fastboot {
        None
    } else {
        Some(build_adb_root_discovery_commands(&state.device_runtime)?)
    };
    let serial = if is_fastboot {
        state.device_runtime.active_fastboot_serial()?
    } else {
        state.device_runtime.active_adb_serial()?
    };
    let result = Arc::new(Mutex::new(None));
    let result_for_command = result.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Discovering,
            partition_refresh_operation_title(is_fastboot),
            move |context, cancellation| async move {
                let parsed = if let Some(command) = fastboot_command {
                    context.report_stage("读取 Fastboot 分区表");
                    let cancellation_for_command = cancellation.clone();
                    let output = task::spawn_blocking(move || {
                        run_command_with_cancel(command, None, move || {
                            cancellation_for_command.is_cancelled()
                        })
                    })
                    .await
                    .map_err(|error| {
                        DomainError::Internal(format!("分区表读取调度失败：{error}"))
                    })??;
                    if output.exit_code != 0 {
                        return Err(DomainError::ExternalTool(format!(
                            "读取 Fastboot 分区表失败，退出码 {}：{}",
                            output.exit_code, output.stderr
                        )));
                    }
                    parse_fastboot_partition_table(
                        &serial,
                        &format!("{}\n{}", output.stdout, output.stderr),
                    )?
                } else {
                    context.report_stage("检查 ADB Root 并读取分区表");
                    let commands = adb_root_commands.expect("ADB Root commands should exist");
                    let cancellation_for_commands = cancellation.clone();
                    let outputs = task::spawn_blocking(move || {
                        commands
                            .into_iter()
                            .map(|command| {
                                let cancellation = cancellation_for_commands.clone();
                                run_command_with_cancel(command, None, move || {
                                    cancellation.is_cancelled()
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .await
                    .map_err(|error| {
                        DomainError::Internal(format!("ADB Root 分区表读取调度失败：{error}"))
                    })??;
                    if let Some(output) = outputs.iter().find(|output| output.exit_code != 0) {
                        return Err(DomainError::ExternalTool(format!(
                            "ADB Root 分区表读取失败，退出码 {}：{}",
                            output.exit_code, output.stderr
                        )));
                    }
                    if outputs[0].stdout.trim() != "0" {
                        return Err(DomainError::InvalidOperation(
                            "ADB 设备未授予 Root 权限。".to_string(),
                        ));
                    }
                    parse_adb_root_partition_table(&serial, &outputs[1].stdout, &outputs[2].stdout)?
                };
                *result_for_command
                    .lock()
                    .expect("partition refresh result lock should not be poisoned") = Some(parsed);
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let snapshot = result
        .lock()
        .expect("partition refresh result lock should not be poisoned")
        .clone()
        .ok_or_else(|| "分区表读取未返回结果。".to_string());
    let snapshot = snapshot?;
    state.partition_workspace.apply_snapshot(snapshot.clone());
    Ok(snapshot)
}

#[tauri::command]
pub fn partitions_prepare_erase(
    state: State<'_, AppState>,
    selected_names: Vec<String>,
) -> Result<PartitionEraseConfirmationDto, String> {
    state
        .partition_workspace
        .build_erase_plan(&selected_names)?;
    state
        .partition_workspace
        .selection_summary(&selected_names)
        .map(confirmation_from_summary)
}

#[tauri::command]
pub fn partitions_map_images(
    state: State<'_, AppState>,
    image_paths: Vec<String>,
) -> Result<PartitionImageMappingDto, String> {
    let images = image_paths
        .iter()
        .map(|path| crate::commands::quick_flash::inspect_image_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let mapped = state.partition_workspace.map_images(&images);
    Ok(PartitionImageMappingDto {
        mapped_count: mapped.len(),
    })
}

#[tauri::command]
pub fn partitions_prepare_write(
    state: State<'_, AppState>,
    selected_names: Vec<String>,
) -> Result<PartitionEraseConfirmationDto, String> {
    state
        .partition_workspace
        .build_write_plan(&selected_names)?;
    state
        .partition_workspace
        .selection_summary(&selected_names)
        .map(confirmation_from_summary)
}

#[tauri::command]
pub fn partitions_prepare_backup(
    state: State<'_, AppState>,
    selected_names: Vec<String>,
    output_directory: String,
) -> Result<PartitionEraseConfirmationDto, String> {
    state
        .partition_workspace
        .build_backup_plan(&selected_names, &output_directory)?;
    state
        .partition_workspace
        .selection_summary(&selected_names)
        .map(confirmation_from_summary)
}

#[tauri::command]
pub async fn partitions_execute_erase(
    state: State<'_, AppState>,
    selected_names: Vec<String>,
) -> Result<crate::commands::quick_flash::CommandExecutionResultDto, String> {
    let plan = state
        .partition_workspace
        .build_erase_plan(&selected_names)?;
    crate::commands::quick_flash::quick_flash_execute_commands(state, plan, false).await
}

#[tauri::command]
pub async fn partitions_execute_write(
    state: State<'_, AppState>,
    selected_names: Vec<String>,
) -> Result<crate::commands::quick_flash::CommandExecutionResultDto, String> {
    let plan = state
        .partition_workspace
        .build_write_plan(&selected_names)?;
    crate::commands::quick_flash::quick_flash_execute_commands(state, plan, false).await
}

#[tauri::command]
pub async fn partitions_execute_backup(
    state: State<'_, AppState>,
    selected_names: Vec<String>,
    output_directory: String,
) -> Result<crate::commands::quick_flash::CommandExecutionResultDto, String> {
    let plan = state
        .partition_workspace
        .build_backup_plan(&selected_names, &output_directory)?;
    let total = plan.tasks.len();
    let device_runtime = state.device_runtime.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Transferring,
            format!("备份分区({total}项)"),
            move |context, cancellation| async move {
                for (index, task) in plan.tasks.iter().enumerate() {
                    context
                        .report_partition_task(
                            task.partition_name.clone(),
                            nwflash_domain::PartitionTaskState::Running,
                            index as f64 / total as f64,
                        )
                        .await;
                    context.report_stage(format!("校验 ADB Root 分区 {}", task.partition_name));
                    let task_result: Result<(), DomainError> = async {
                        let resolution_command = build_adb_root_path_resolution_command(
                            &device_runtime,
                            &task.partition_name,
                        )
                        .map_err(DomainError::InvalidOperation)?;
                        let cancellation_for_resolution = cancellation.clone();
                        let resolved = task::spawn_blocking(move || {
                            run_command_with_cancel(resolution_command, None, move || {
                                cancellation_for_resolution.is_cancelled()
                            })
                        })
                        .await
                        .map_err(|error| {
                            DomainError::Internal(format!("备份分区校验调度失败：{error}"))
                        })??;
                        if resolved.exit_code != 0 || resolved.stdout.trim() != task.device_path {
                            return Err(DomainError::InvalidOperation(
                                "分区设备路径已变化，请重新读取分区表后再执行。".to_string(),
                            ));
                        }

                        let current_serial = device_runtime
                            .active_adb_serial()
                            .map_err(DomainError::InvalidOperation)?;
                        let (command, partial_path) =
                            build_adb_root_backup_command(&current_serial, task)
                                .map_err(DomainError::InvalidOperation)?;
                        let partial_path_for_finalization = partial_path.clone();
                        let partial_path_for_cleanup = partial_path.clone();
                        let output_path = PathBuf::from(
                            task.output_path
                                .as_ref()
                                .expect("backup plan task should have output path"),
                        );
                        if let Some(parent) = partial_path.parent() {
                            std::fs::create_dir_all(parent).map_err(|error| {
                                DomainError::InvalidOperation(format!("无法创建备份目录：{error}"))
                            })?;
                        }
                        context.report_stage(format!("备份 {}", task.partition_name));
                        let cancellation_for_backup = cancellation.clone();
                        let output = match task::spawn_blocking(move || {
                            run_command_with_file_stdout_and_cancel(
                                command,
                                &partial_path,
                                None,
                                move || cancellation_for_backup.is_cancelled(),
                            )
                        })
                        .await
                        {
                            Ok(Ok(output)) => output,
                            Ok(Err(error)) => {
                                discard_backup_partial(&partial_path_for_cleanup);
                                return Err(error);
                            }
                            Err(error) => {
                                discard_backup_partial(&partial_path_for_cleanup);
                                return Err(DomainError::Internal(format!(
                                    "备份分区调度失败：{error}"
                                )));
                            }
                        };
                        if output.exit_code != 0 {
                            discard_backup_partial(&partial_path_for_cleanup);
                            return Err(DomainError::ExternalTool(format!(
                                "分区备份失败，退出码 {}：{}",
                                output.exit_code, output.stderr
                            )));
                        }
                        if let Err(error) = finalize_backup_file(
                            &partial_path_for_finalization,
                            &output_path,
                            task.size_bytes,
                        ) {
                            discard_backup_partial(&partial_path_for_cleanup);
                            return Err(DomainError::InvalidOperation(error));
                        }
                        Ok(())
                    }
                    .await;

                    match task_result {
                        Ok(()) => {
                            context.report_progress((index + 1) as f64 / total as f64);
                            context
                                .report_partition_task(
                                    task.partition_name.clone(),
                                    nwflash_domain::PartitionTaskState::Succeeded,
                                    (index + 1) as f64 / total as f64,
                                )
                                .await;
                        }
                        Err(error) => {
                            crate::commands::quick_flash::report_partition_terminal_updates(
                                &context,
                                &plan.tasks,
                                index,
                                &error,
                            )
                            .await;
                            return Err(error);
                        }
                    }
                }
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    Ok(crate::commands::quick_flash::CommandExecutionResultDto {
        command_count: total,
        executed_count: total,
    })
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
    fn refresh_plan_uses_only_the_current_fastboot_snapshot() {
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

        let command = build_partition_refresh_plan(&runtime)
            .expect("current Fastboot snapshot should build a partition refresh command");

        assert_eq!(
            command.program,
            nwflash_windows::bundled_platform_tool("fastboot.exe")
        );
        assert_eq!(command.args, vec!["-s", "FAST-1", "getvar", "all"]);
    }

    #[test]
    fn requested_fastboot_transport_requires_the_current_fastboot_snapshot() {
        let runtime = DeviceRuntime::new();
        runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::AdbConnected,
                serial: "ADB-1".to_string(),
                connection_label: "ADB 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );

        let error = resolve_partition_refresh_transport(&runtime, PartitionTransportKind::Fastboot)
            .expect_err("a forced Fastboot refresh must reject an ADB snapshot");

        assert!(error.contains("Fastboot"));
    }

    #[test]
    fn requested_adb_root_transport_requires_the_current_adb_snapshot() {
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

        let error = resolve_partition_refresh_transport(&runtime, PartitionTransportKind::AdbRoot)
            .expect_err("a forced ADB Root refresh must reject a Fastboot snapshot");

        assert!(error.contains("ADB"));
    }

    #[test]
    fn automatic_transport_resolves_the_current_device_mode() {
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

        assert_eq!(
            resolve_partition_refresh_transport(&runtime, PartitionTransportKind::Automatic)
                .expect("automatic transport should resolve Fastboot"),
            PartitionTransportKind::Fastboot,
        );
    }

    #[test]
    fn adb_root_discovery_commands_use_only_the_current_adb_snapshot() {
        let runtime = DeviceRuntime::new();
        runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::AdbConnected,
                serial: "ADB-1".to_string(),
                connection_label: "ADB 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );

        let commands = build_adb_root_discovery_commands(&runtime)
            .expect("the current ADB snapshot should build Root discovery commands");

        assert_eq!(commands.len(), 3);
        assert_eq!(
            commands[0].program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            commands[0].args,
            vec!["-s", "ADB-1", "shell", "-T", "su", "--no-pty", "-c", "'id -u'"]
        );
        assert_eq!(
            commands[1].args,
            vec![
                "-s",
                "ADB-1",
                "shell",
                "-T",
                "su",
                "--no-pty",
                "-c",
                "'getprop ro.boot.slot_suffix'"
            ]
        );
        assert_eq!(
            commands[2].args[0..7],
            ["-s", "ADB-1", "shell", "-T", "su", "--no-pty", "-c"]
        );
        let discovery_script = commands[2].args.last().expect("discovery script");
        assert!(discovery_script.starts_with("'for d in "));
        assert!(discovery_script.ends_with("done'"));
        assert!(discovery_script.contains("for d in /dev/block/by-name"));
        assert!(discovery_script.contains("; do "));
        assert!(discovery_script.contains("; done'"));
    }

    #[test]
    fn refresh_operation_title_names_the_selected_transport() {
        assert_eq!(
            partition_refresh_operation_title(true),
            "读取 Fastboot 分区表"
        );
        assert_eq!(
            partition_refresh_operation_title(false),
            "读取 ADB Root 分区表"
        );
    }

    #[test]
    fn adb_root_path_resolution_uses_the_current_serial_and_rejects_shell_input() {
        let runtime = DeviceRuntime::new();
        runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::AdbConnected,
                serial: "ADB-2".to_string(),
                connection_label: "ADB 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );

        let command = build_adb_root_path_resolution_command(&runtime, "boot_a")
            .expect("a discovered partition name should build a resolve command");

        assert_eq!(command.args[0..2], ["-s", "ADB-2"]);
        assert_eq!(command.args[2..7], ["shell", "-T", "su", "--no-pty", "-c"]);
        let resolve_script = command.args.last().expect("resolve script");
        assert!(resolve_script.starts_with("'for d in "));
        assert!(resolve_script.contains("$d/boot_a"));
        assert!(resolve_script.ends_with("done'"));
        assert!(build_adb_root_path_resolution_command(&runtime, "boot_a;reboot").is_err());
    }

    #[test]
    fn workspace_runtime_prepares_an_erase_plan_only_for_names_in_its_snapshot() {
        let runtime = PartitionWorkspaceRuntime::new();
        runtime.apply_snapshot(
            parse_fastboot_partition_table(
                "FAST-1",
                "current-slot: a\npartition-size:super:0x200000000\n",
            )
            .expect("fixture should parse"),
        );

        let plan = runtime
            .build_erase_plan(&["super".to_string()])
            .expect("known selected name should build an erase plan");

        assert_eq!(plan.tasks[0].partition_name, "super");
    }

    #[test]
    fn workspace_runtime_builds_adb_root_backup_only_from_its_snapshot() {
        let runtime = PartitionWorkspaceRuntime::new();
        runtime.apply_snapshot(
            parse_adb_root_partition_table("ADB-1", "a", "boot_a|/dev/block/sda12|64|0\n")
                .expect("fixture should parse"),
        );

        let plan = runtime
            .build_backup_plan(&["boot_a".to_string()], r"C:\backups")
            .expect("an ADB Root snapshot should build a backup plan");

        assert_eq!(
            plan.operation,
            nwflash_domain::PartitionOperationKind::Backup
        );
        assert_eq!(
            plan.tasks[0].output_path.as_deref(),
            Some(r"C:\backups\boot_a.img")
        );
    }

    #[test]
    fn backup_finalization_rejects_a_truncated_partial_without_overwriting_the_last_backup() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-backup-{nonce}"));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let output = root.join("boot_a.img");
        let partial = root.join("boot_a.img.partial");
        fs::write(&output, b"previous-good-backup").expect("old backup should be written");
        fs::write(&partial, [0x01]).expect("partial should be written");

        let error = finalize_backup_file(&partial, &output, Some(64))
            .expect_err("a truncated backup must be rejected");

        assert!(error.contains("大小不符"));
        assert_eq!(
            fs::read(&output).expect("old backup should remain"),
            b"previous-good-backup"
        );
        assert!(!partial.exists());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn adb_root_backup_command_writes_to_a_partial_file_before_finalization() {
        let task = nwflash_domain::PartitionTask {
            partition_name: "boot_a".to_string(),
            device_path: "/dev/block/sda12".to_string(),
            image_path: None,
            output_path: Some(r"C:\backups\boot_a.img".to_string()),
            size_bytes: Some(64),
        };

        let (command, partial) = build_adb_root_backup_command("ADB-1", &task)
            .expect("an ADB Root backup task should build");

        assert_eq!(
            partial,
            std::path::PathBuf::from(r"C:\backups\boot_a.img.partial")
        );
        assert_eq!(
            command.program,
            nwflash_windows::bundled_platform_tool("adb.exe")
        );
        assert_eq!(
            command.args[0..6],
            ["-s", "ADB-1", "exec-out", "su", "--no-pty", "-c"]
        );
    }

    #[test]
    fn atomic_backup_replace_moves_a_checked_partial_over_the_existing_output() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-backup-replace-{nonce}"));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let output = root.join("boot_a.img");
        let partial = root.join("boot_a.img.partial");
        fs::write(&output, b"old").expect("old backup should be written");
        fs::write(&partial, b"new").expect("partial should be written");

        replace_backup_file_atomically(&partial, &output)
            .expect("checked partial should replace the old backup");

        assert_eq!(fs::read(&output).expect("output should remain"), b"new");
        assert!(!partial.exists());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn backup_process_failure_removes_its_partial_output() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-backup-cleanup-{nonce}"));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let partial = root.join("boot_a.img.partial");
        fs::write(&partial, b"incomplete").expect("partial should be written");

        discard_backup_partial(&partial);

        assert!(!partial.exists());
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn erase_confirmation_marks_high_risk_snapshot_partitions_without_exposing_paths() {
        let runtime = PartitionWorkspaceRuntime::new();
        runtime.apply_snapshot(
            parse_fastboot_partition_table("FAST-1", "partition-size:super:0x200000000\n")
                .expect("fixture should parse"),
        );
        let confirmation = confirmation_from_summary(
            runtime
                .selection_summary(&["super".to_string()])
                .expect("known selected name should produce a summary"),
        );

        assert_eq!(confirmation.task_count, 1);
        assert_eq!(confirmation.high_risk_count, 1);
        assert_eq!(confirmation.mounted_count, 0);
    }

    #[test]
    fn runtime_selection_summary_preserves_mounted_risk_without_exposing_device_paths() {
        let runtime = PartitionWorkspaceRuntime::new();
        runtime.apply_snapshot(
            parse_adb_root_partition_table(
                "ADB-1",
                "a",
                "boot_a|/dev/block/sda12|64|0\nsuper|/dev/block/sda70|8589934592|1\n",
            )
            .expect("fixture should parse"),
        );

        let summary = runtime
            .selection_summary(&["boot_a".to_string(), "super".to_string()])
            .expect("known snapshot selection should be summarized");

        assert_eq!(summary.task_count, 2);
        assert_eq!(summary.high_risk_count, 1);
        assert_eq!(summary.mounted_count, 1);
    }
}
