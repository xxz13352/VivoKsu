use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use nwflash_application::{
    parse_remote_listing, result_to_domain_error, CommandSpec, FileManagerService,
    OperationCoordinator, OperationCoordinatorError,
};
use nwflash_domain::{DeviceFileEntry, DomainError, OperationKind};
use nwflash_windows::{
    file_ops::{
        create_exclusive_regular_file, ensure_safe_directory, ensure_safe_regular_file,
        open_regular_file_no_follow, promote_without_replace,
    },
    process::{run_command_with_cancel, CancellableProcessExecutor, ProcessCommand, ProcessOutput},
};
use tauri::State;
use tokio::task;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::commands::device::DeviceRuntime;
use crate::AppState;

#[cfg(feature = "e2e")]
pub(crate) mod e2e;

const FILE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const APK_INSTALL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const FILE_PROMOTE_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
enum FileTransaction {
    Upload {
        service: FileManagerService,
        serial: String,
        source_path: PathBuf,
        remote_directory: String,
        remote_destination: String,
    },
    Download {
        service: FileManagerService,
        serial: String,
        remote_source: String,
        local_destination: PathBuf,
        remote_size: Option<u64>,
    },
    Install {
        command: CommandSpec,
    },
}

/// 下载进度回调：参数为 0.0~1.0 的已完成比例。
/// 由 `execute_file_transaction_with_executor` 提供，内部把比例上报到
/// 操作协调器的快照 `progress`（操作日志区的进度行据此实时刷新）。
type FileProgressSink = Arc<dyn Fn(f64) + Send + Sync>;

/// 仅测试使用：无进度回调的默认 sink。
#[cfg(test)]
fn no_progress() -> FileProgressSink {
    Arc::new(|_| {})
}

impl FileTransaction {
    /// 仅测试使用：生产路径一律走 `execute_with_progress`。
    #[cfg(test)]
    fn execute(
        self,
        executor: &dyn CancellableProcessExecutor,
        cancellation: &CancellationToken,
    ) -> Result<(), DomainError> {
        self.execute_with_progress(executor, cancellation, &no_progress())
    }

    fn execute_with_progress(
        self,
        executor: &dyn CancellableProcessExecutor,
        cancellation: &CancellationToken,
        progress: &FileProgressSink,
    ) -> Result<(), DomainError> {
        match self {
            Self::Upload {
                service,
                serial,
                source_path,
                remote_directory,
                remote_destination,
            } => execute_upload_transaction(
                &service,
                &serial,
                &source_path,
                &remote_directory,
                &remote_destination,
                executor,
                cancellation,
            ),
            Self::Download {
                service,
                serial,
                remote_source,
                local_destination,
                remote_size,
            } => execute_download_transaction(
                &service,
                &serial,
                &remote_source,
                &local_destination,
                executor,
                cancellation,
                remote_size,
                progress,
            ),
            Self::Install { command } => {
                execute_install_transaction(into_process_command(command), executor, cancellation)
            }
        }
    }
}

fn into_process_command(command: CommandSpec) -> ProcessCommand {
    ProcessCommand {
        program: command.program,
        args: command.args,
        working_directory: command.working_directory,
        environment: command.environment,
    }
}

fn run_process(
    executor: &dyn CancellableProcessExecutor,
    command: ProcessCommand,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, DomainError> {
    let mut should_cancel = || cancellation.is_cancelled();
    executor.run_with_timeout(command, Some(timeout), &mut should_cancel)
}

fn contextual_process_error(error: DomainError, label: &str) -> DomainError {
    match error {
        // Cancellation is interpreted by the coordinator and must retain its
        // dedicated terminal state.  Other executor details (including raw
        // stderr, spawn paths, and platform-specific timeout text) stay out
        // of the file-operation error surface.
        DomainError::UserCancelled(_) => error,
        _ => DomainError::ExternalTool(format!("{label}执行失败。")),
    }
}

fn run_cleanup_process(
    executor: &dyn CancellableProcessExecutor,
    command: ProcessCommand,
) -> Result<ProcessOutput, DomainError> {
    let mut never_cancel = || false;
    executor.run_with_timeout(command, Some(FILE_CLEANUP_TIMEOUT), &mut never_cancel)
}

fn require_success(output: ProcessOutput, label: &str) -> Result<(), DomainError> {
    if output.exit_code == 0 {
        Ok(())
    } else {
        Err(DomainError::ExternalTool(format!(
            "{label}失败，退出码 {}。",
            output.exit_code
        )))
    }
}

fn cancellation_error() -> DomainError {
    DomainError::UserCancelled("运行被用户取消".to_string())
}

fn cleanup_failure(primary: DomainError) -> DomainError {
    match primary {
        // Once cleanup cannot be confirmed, the operation is failed even if
        // cancellation was the primary trigger.  Reporting only "canceled"
        // would hide a transaction-owned partial file that may still exist.
        DomainError::UserCancelled(_) => {
            DomainError::ExternalTool("运行已取消，但临时文件清理未确认。".to_string())
        }
        _ => DomainError::ExternalTool("文件传输清理未确认。".to_string()),
    }
}

fn return_after_cleanup(
    primary: DomainError,
    cleanup: impl FnOnce() -> Result<(), DomainError>,
) -> Result<(), DomainError> {
    match cleanup() {
        Ok(()) => Err(primary),
        Err(_) => Err(cleanup_failure(primary)),
    }
}

fn cleanup_remote_temp(
    service: &FileManagerService,
    serial: &str,
    temporary_path: &str,
    executor: &dyn CancellableProcessExecutor,
) -> Result<(), DomainError> {
    let command = service.build_remote_remove_file_command(serial, temporary_path)?;
    require_success(
        run_cleanup_process(executor, into_process_command(command))?,
        "清理远端暂存文件",
    )
}

fn cleanup_local_temp(path: &Path) -> Result<(), DomainError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(DomainError::ExternalTool(
            "清理本地暂存文件失败。".to_string(),
        )),
    }
}

fn remote_temporary_path(remote_directory: &str) -> String {
    let directory = remote_directory.trim_end_matches('/');
    if directory.is_empty() {
        format!("/.nwflash-upload-{}.partial", Uuid::new_v4().simple())
    } else {
        format!(
            "{directory}/.nwflash-upload-{}.partial",
            Uuid::new_v4().simple()
        )
    }
}

fn ensure_local_destination_absent(path: &Path) -> Result<(), DomainError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(DomainError::InvalidOperation(
            "下载目标已存在，请选择新的文件名。".to_string(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(DomainError::InvalidInput(
            "下载目标路径不可用。".to_string(),
        )),
    }
}

fn reserve_local_temporary_path(destination: &Path) -> Result<PathBuf, DomainError> {
    let parent = destination
        .parent()
        .ok_or_else(|| DomainError::InvalidInput("下载目标路径必须包含父目录。".to_string()))?;
    ensure_safe_directory(parent)
        .map_err(|_| DomainError::InvalidInput("下载目标目录不可安全使用。".to_string()))?;
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| DomainError::InvalidInput("下载目标文件名无效。".to_string()))?;

    for _ in 0..8 {
        let candidate = parent.join(format!(
            ".{name}.nwflash-download-{}.partial",
            Uuid::new_v4().simple()
        ));
        match create_exclusive_regular_file(&candidate) {
            Ok(file) => {
                if let Err(error) = file.sync_all() {
                    drop(file);
                    let _ = fs::remove_file(&candidate);
                    return Err(DomainError::ExternalTool(format!(
                        "创建下载暂存文件失败：{error}"
                    )));
                }
                drop(file);
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(DomainError::ExternalTool(format!(
                    "创建下载暂存文件失败：{error}"
                )))
            }
        }
    }

    Err(DomainError::ExternalTool(
        "无法生成唯一的下载暂存文件。".to_string(),
    ))
}

fn sync_and_validate_local_temporary(path: &Path) -> Result<(), DomainError> {
    // FlushFileBuffers on Windows requires a writable handle.  The pull
    // process has already closed its writer, so opening read/write here gives
    // us a portable durability check without changing the file contents.
    let file = open_regular_file_no_follow(path)
        .map_err(|_| DomainError::ExternalTool("读取下载暂存文件失败。".to_string()))?;
    file.sync_all()
        .map_err(|_| DomainError::ExternalTool("同步下载暂存文件失败。".to_string()))?;
    ensure_safe_regular_file(path)
        .map_err(|_| DomainError::ExternalTool("下载暂存结果不是普通文件。".to_string()))?;
    Ok(())
}

fn promote_local_temporary(temporary: &Path, destination: &Path) -> Result<(), DomainError> {
    promote_without_replace(temporary, destination).map_err(|error| {
        // A destination can appear after the initial reservation check.  On
        // Windows MoveFileExW may report that race as either
        // ERROR_ALREADY_EXISTS or ERROR_ACCESS_DENIED (for example when the
        // competing handle is open), so inspect it without following links to
        // preserve the explicit no-replace conflict classification.
        if matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
        ) && fs::symlink_metadata(destination).is_ok()
        {
            DomainError::InvalidOperation("下载目标已存在，请选择新的文件名。".to_string())
        } else {
            DomainError::ExternalTool("提交下载文件失败。".to_string())
        }
    })
}

fn execute_upload_transaction(
    service: &FileManagerService,
    serial: &str,
    source_path: &Path,
    remote_directory: &str,
    remote_destination: &str,
    executor: &dyn CancellableProcessExecutor,
    cancellation: &CancellationToken,
) -> Result<(), DomainError> {
    if cancellation.is_cancelled() {
        return Err(cancellation_error());
    }

    let temporary_path = remote_temporary_path(remote_directory);
    let transfer = service
        .build_push_command_to_path(serial, source_path, &temporary_path)
        .map(into_process_command)?;
    let transfer_result = run_process(executor, transfer, FILE_TRANSFER_TIMEOUT, cancellation)
        .map_err(|error| contextual_process_error(error, "上传文件"))
        .and_then(|output| require_success(output, "上传文件"));
    if let Err(primary) = transfer_result {
        return return_after_cleanup(primary, || {
            cleanup_remote_temp(service, serial, &temporary_path, executor)
        });
    }

    if cancellation.is_cancelled() {
        return return_after_cleanup(cancellation_error(), || {
            cleanup_remote_temp(service, serial, &temporary_path, executor)
        });
    }

    let promote = service
        .build_remote_promote_no_replace_command(serial, &temporary_path, remote_destination)
        .map(into_process_command)
        .and_then(|command| {
            run_process(executor, command, FILE_PROMOTE_TIMEOUT, cancellation)
                .map_err(|error| contextual_process_error(error, "提交远端文件"))
        })
        .and_then(|output| require_success(output, "提交远端文件"));
    match promote {
        Ok(()) => Ok(()),
        Err(primary) => return_after_cleanup(primary, || {
            cleanup_remote_temp(service, serial, &temporary_path, executor)
        }),
    }
}

/// 下载进度轮询器：pull 阻塞运行期间，在独立线程上按固定间隔采样本地
/// 临时文件的大小，按“已落盘字节 / 远端总字节”上报比例。pull 结束后
/// 必须 `stop_and_join`，保证线程不泄漏。
struct LocalProgressPoller {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl LocalProgressPoller {
    fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn spawn_local_progress_poller(
    temporary_path: PathBuf,
    remote_size: u64,
    progress: FileProgressSink,
    cancellation: CancellationToken,
) -> LocalProgressPoller {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = stop.clone();
    let join = std::thread::Builder::new()
        .name("file-download-progress".to_string())
        .spawn(move || {
            let mut last_reported = 0.0_f64;
            while !stop_for_thread.load(Ordering::Relaxed) {
                if cancellation.is_cancelled() {
                    break;
                }
                if let Ok(metadata) = fs::metadata(&temporary_path) {
                    let fraction = if remote_size == 0 {
                        0.0
                    } else {
                        (metadata.len() as f64 / remote_size as f64).clamp(0.0, 0.99)
                    };
                    if fraction - last_reported >= 0.01 {
                        last_reported = fraction;
                        progress(fraction);
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        })
        .ok();
    LocalProgressPoller { stop, join }
}

#[allow(clippy::too_many_arguments)]
fn execute_download_transaction(
    service: &FileManagerService,
    serial: &str,
    remote_source: &str,
    local_destination: &Path,
    executor: &dyn CancellableProcessExecutor,
    cancellation: &CancellationToken,
    remote_size: Option<u64>,
    progress: &FileProgressSink,
) -> Result<(), DomainError> {
    if cancellation.is_cancelled() {
        return Err(cancellation_error());
    }
    ensure_local_destination_absent(local_destination)?;
    let temporary_path = reserve_local_temporary_path(local_destination)?;
    let cleanup = || cleanup_local_temp(&temporary_path);

    let transfer = service
        .build_pull_command_to_path(serial, remote_source, &temporary_path)
        .map(into_process_command);
    let transfer_result = match transfer {
        Ok(command) => {
            let poller = remote_size.map(|size| {
                spawn_local_progress_poller(
                    temporary_path.clone(),
                    size,
                    progress.clone(),
                    cancellation.clone(),
                )
            });
            let result = run_process(executor, command, FILE_TRANSFER_TIMEOUT, cancellation)
                .map_err(|error| contextual_process_error(error, "下载文件"))
                .and_then(|output| require_success(output, "下载文件"));
            if let Some(poller) = poller {
                poller.stop_and_join();
            }
            result
        }
        Err(error) => Err(error),
    };
    if let Err(primary) = transfer_result {
        return return_after_cleanup(primary, cleanup);
    }

    if cancellation.is_cancelled() {
        return return_after_cleanup(cancellation_error(), cleanup);
    }
    if let Err(primary) = sync_and_validate_local_temporary(&temporary_path) {
        return return_after_cleanup(primary, cleanup);
    }
    match promote_local_temporary(&temporary_path, local_destination) {
        Ok(()) => Ok(()),
        Err(primary) => return_after_cleanup(primary, cleanup),
    }
}

fn execute_install_transaction(
    command: ProcessCommand,
    executor: &dyn CancellableProcessExecutor,
    cancellation: &CancellationToken,
) -> Result<(), DomainError> {
    if cancellation.is_cancelled() {
        return Err(cancellation_error());
    }
    run_process(executor, command, APK_INSTALL_TIMEOUT, cancellation)
        .map_err(|error| contextual_process_error(error, "安装 APK"))
        .and_then(|output| require_success(output, "安装 APK"))
}

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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

async fn execute_file_transaction(
    state: &AppState,
    transaction: FileTransaction,
    operation_kind: OperationKind,
    operation_title: &'static str,
) -> Result<(), String> {
    let result = execute_file_transaction_with_executor(
        &state.operation_coordinator,
        transaction,
        operation_kind,
        operation_title,
        file_transaction_executor(),
    )
    .await;
    #[cfg(feature = "e2e")]
    e2e::record_terminal(&result);
    result.map_err(|error| result_to_domain_error(error).to_string())
}

fn file_transaction_executor() -> Arc<dyn CancellableProcessExecutor> {
    #[cfg(feature = "e2e")]
    {
        e2e::executor()
    }
    #[cfg(not(feature = "e2e"))]
    {
        Arc::new(nwflash_windows::process::SystemCancellableProcessExecutor)
    }
}

async fn execute_file_transaction_with_executor(
    coordinator: &OperationCoordinator,
    transaction: FileTransaction,
    operation_kind: OperationKind,
    operation_title: &'static str,
    executor: Arc<dyn CancellableProcessExecutor>,
) -> Result<(), OperationCoordinatorError> {
    coordinator
        .run_async(
            operation_kind,
            operation_title,
            move |context, cancellation| async move {
                context.report_stage(operation_title);
                // 下载事务运行在阻塞线程上，无法直接调用异步的快照上报；
                // 通过运行时 Handle 把比例回调转发回协调器（内部按 100ms
                // 节流广播，操作日志区的进度行据此原地刷新）。
                let progress: FileProgressSink = {
                    let runtime = tokio::runtime::Handle::current();
                    let context = context.clone();
                    Arc::new(move |fraction: f64| {
                        let context = context.clone();
                        // fire-and-forget：比例回调无需等待上报完成。
                        drop(runtime.spawn(async move {
                            context.report_progress(fraction);
                        }));
                    })
                };
                task::spawn_blocking(move || {
                    transaction.execute_with_progress(executor.as_ref(), &cancellation, &progress)
                })
                .await
                .map_err(|error| {
                    DomainError::Internal(format!("{operation_title}调度失败：{error}"))
                })??;
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
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
    remote_size: Option<u64>,
) -> Result<(), String> {
    let serial = state.device_runtime.active_adb_serial()?;
    let service = FileManagerService::bundled();
    let destination = PathBuf::from(&destination_path);
    // Keep the existing plan validation at the command boundary; the
    // transaction later adds the no-replace/temporary-file checks after
    // coordinator authorization.
    service
        .build_pull_command(&serial, &remote_path, &destination)
        .map_err(|error| error.to_string())?;
    execute_file_transaction(
        &state,
        FileTransaction::Download {
            service,
            serial,
            remote_source: remote_path,
            local_destination: destination,
            // 前端随目录列表传入的远端文件大小；缺失或为 0 时下载保持
            // 原有行为（无进度上报）。
            remote_size: remote_size.filter(|size| *size > 0),
        },
        OperationKind::Transferring,
        "下载设备文件",
    )
    .await
}

#[tauri::command]
pub async fn files_upload(
    state: State<'_, AppState>,
    source_path: String,
    remote_directory: String,
) -> Result<(), String> {
    let serial = state.device_runtime.active_adb_serial()?;
    let service = FileManagerService::bundled();
    let source = PathBuf::from(&source_path);
    let remote_destination = service
        .push_destination(&serial, &source, &remote_directory)
        .map_err(|error| error.to_string())?;
    execute_file_transaction(
        &state,
        FileTransaction::Upload {
            service,
            serial,
            source_path: source,
            remote_directory,
            remote_destination,
        },
        OperationKind::Transferring,
        "上传设备文件",
    )
    .await
}

#[tauri::command]
pub async fn files_install_apk(state: State<'_, AppState>, apk_path: String) -> Result<(), String> {
    let command = build_install_apk_plan(&state.device_runtime, Path::new(&apk_path))?;
    execute_file_transaction(
        &state,
        FileTransaction::Install { command },
        OperationKind::Installing,
        "安装 APK",
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
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

    #[derive(Clone)]
    struct RecordedFileExecutor {
        commands: Arc<Mutex<Vec<ProcessCommand>>>,
        timeouts: Arc<Mutex<Vec<Option<Duration>>>>,
        outcomes: Arc<Mutex<VecDeque<Result<ProcessOutput, DomainError>>>>,
        pull_bytes: Option<Vec<u8>>,
        pull_conflict: Option<PathBuf>,
        pull_remove_temp: bool,
    }

    impl RecordedFileExecutor {
        fn new(outcomes: impl IntoIterator<Item = Result<ProcessOutput, DomainError>>) -> Self {
            Self {
                commands: Arc::new(Mutex::new(Vec::new())),
                timeouts: Arc::new(Mutex::new(Vec::new())),
                outcomes: Arc::new(Mutex::new(outcomes.into_iter().collect())),
                pull_bytes: None,
                pull_conflict: None,
                pull_remove_temp: false,
            }
        }

        fn with_pull_bytes(mut self, bytes: impl Into<Vec<u8>>) -> Self {
            self.pull_bytes = Some(bytes.into());
            self
        }

        fn with_pull_conflict(mut self, destination: PathBuf) -> Self {
            self.pull_conflict = Some(destination);
            self
        }

        fn with_pull_remove_temp(mut self) -> Self {
            self.pull_remove_temp = true;
            self
        }

        fn commands(&self) -> Vec<ProcessCommand> {
            self.commands
                .lock()
                .expect("file executor command lock should not be poisoned")
                .clone()
        }

        fn timeouts(&self) -> Vec<Option<Duration>> {
            self.timeouts
                .lock()
                .expect("file executor timeout lock should not be poisoned")
                .clone()
        }
    }

    impl CancellableProcessExecutor for RecordedFileExecutor {
        fn run(
            &self,
            command: ProcessCommand,
            should_cancel: &mut dyn FnMut() -> bool,
        ) -> Result<ProcessOutput, DomainError> {
            self.run_with_timeout(command, None, should_cancel)
        }

        fn run_with_timeout(
            &self,
            command: ProcessCommand,
            timeout: Option<Duration>,
            should_cancel: &mut dyn FnMut() -> bool,
        ) -> Result<ProcessOutput, DomainError> {
            if should_cancel() {
                return Err(cancellation_error());
            }
            self.commands
                .lock()
                .expect("file executor command lock should not be poisoned")
                .push(command.clone());
            self.timeouts
                .lock()
                .expect("file executor timeout lock should not be poisoned")
                .push(timeout);
            let outcome = self
                .outcomes
                .lock()
                .expect("file executor outcome lock should not be poisoned")
                .pop_front()
                .expect("file executor needs one outcome per command");
            if outcome.is_ok() && command.args.get(2).is_some_and(|arg| arg == "pull") {
                if let Some(destination) = command.args.last() {
                    if let Some(bytes) = &self.pull_bytes {
                        fs::write(destination, bytes).expect("pull fixture should write temp");
                    }
                }
                if let Some(conflict) = &self.pull_conflict {
                    fs::write(conflict, b"existing").expect("conflict fixture should write");
                }
                if self.pull_remove_temp {
                    if let Some(destination) = command.args.last() {
                        let _ = fs::remove_file(destination);
                    }
                }
            }
            outcome
        }
    }

    fn ok_output() -> Result<ProcessOutput, DomainError> {
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn failed_output(exit_code: i32) -> Result<ProcessOutput, DomainError> {
        Ok(ProcessOutput {
            exit_code,
            stdout: String::new(),
            stderr: "fixture failure".to_string(),
        })
    }

    fn external_error(message: &str) -> Result<ProcessOutput, DomainError> {
        Err(DomainError::ExternalTool(message.to_string()))
    }

    fn cancelled_output() -> Result<ProcessOutput, DomainError> {
        Err(cancellation_error())
    }

    fn upload_fixture(root: &std::path::Path) -> (FileTransaction, PathBuf) {
        let source = root.join("payload.bin");
        fs::write(&source, b"upload bytes").expect("upload source should be written");
        let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");
        let serial = "RF8T123".to_string();
        let remote_directory = "/sdcard/Download".to_string();
        let remote_destination = service
            .push_destination(&serial, &source, &remote_directory)
            .expect("upload destination should build");
        (
            FileTransaction::Upload {
                service,
                serial,
                source_path: source.clone(),
                remote_directory,
                remote_destination,
            },
            source,
        )
    }

    fn download_fixture(root: &std::path::Path) -> (FileTransaction, PathBuf) {
        let destination = root.join("downloaded.bin");
        let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");
        (
            FileTransaction::Download {
                service,
                serial: "RF8T123".to_string(),
                remote_source: "/sdcard/Download/payload.bin".to_string(),
                local_destination: destination.clone(),
                remote_size: None,
            },
            destination,
        )
    }

    #[test]
    fn download_progress_poller_reports_monotonic_partial_fractions_and_stops() {
        let root = temporary_directory("download-progress");
        let temp_file = root.join("partial.bin");
        let reported = Arc::new(Mutex::new(Vec::<f64>::new()));
        let reported_for_sink = reported.clone();
        let progress: FileProgressSink = Arc::new(move |fraction: f64| {
            reported_for_sink
                .lock()
                .expect("progress collector lock should not be poisoned")
                .push(fraction);
        });

        let poller = spawn_local_progress_poller(
            temp_file.clone(),
            2048,
            progress,
            CancellationToken::new(),
        );
        let wait_for_fraction = |target: f64| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while std::time::Instant::now() < deadline {
                let collected = reported
                    .lock()
                    .expect("progress collector lock should not be poisoned")
                    .clone();
                if collected.last().is_some_and(|fraction| *fraction >= target) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("poller should report fraction {target} within timeout");
        };

        fs::write(&temp_file, vec![0u8; 512]).expect("quarter chunk should be written");
        wait_for_fraction(0.25);
        fs::write(&temp_file, vec![0u8; 1536]).expect("three-quarter chunk should be written");
        wait_for_fraction(0.75);
        poller.stop_and_join();

        let collected = reported
            .lock()
            .expect("progress collector lock should not be poisoned")
            .clone();
        assert!(
            collected.windows(2).all(|pair| pair[0] < pair[1]),
            "fractions must be strictly increasing: {collected:?}"
        );
        assert!(
            collected
                .iter()
                .all(|fraction| (0.0..0.99).contains(fraction)),
            "fractions must stay below the final promote step: {collected:?}"
        );
        fs::remove_dir_all(root).ok();
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

    #[test]
    fn upload_transaction_pushes_to_owned_temp_then_promotes_without_cleanup() {
        let root = temporary_directory("upload-success");
        let (transaction, source) = upload_fixture(&root);
        let executor = RecordedFileExecutor::new([ok_output(), ok_output()]);

        transaction
            .execute(&executor, &CancellationToken::new())
            .expect("successful upload transaction should complete");

        let commands = executor.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].args.get(2).map(String::as_str), Some("push"));
        let temporary = commands[0].args.last().expect("push temp path");
        assert!(temporary.contains("/.nwflash-upload-"));
        assert!(temporary.ends_with(".partial"));
        assert_eq!(commands[1].args.get(2).map(String::as_str), Some("shell"));
        let script = commands[1].args.last().expect("promote script");
        assert!(script.contains("mv -n --"));
        assert!(script.contains("[ ! -f ") && script.contains("exit 72"));
        assert!(script.contains(
            "[ -e '/sdcard/Download/payload.bin' ] || [ -L '/sdcard/Download/payload.bin' ]"
        ));
        assert!(!commands
            .iter()
            .any(|command| { command.args.iter().any(|argument| argument == "rm") }));
        assert_eq!(
            executor.timeouts(),
            vec![Some(FILE_TRANSFER_TIMEOUT), Some(FILE_PROMOTE_TIMEOUT)]
        );
        assert_eq!(
            fs::read(source).expect("source should remain"),
            b"upload bytes"
        );
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn upload_transfer_failures_cleanup_temp_and_never_promote() {
        let failures = [
            ("nonzero", failed_output(7)),
            ("spawn", external_error("spawn failed")),
            ("timeout", external_error("命令执行超时")),
            ("output", external_error("命令输出超过安全上限")),
            ("read", external_error("读取命令 stderr 输出失败")),
            ("cancel", cancelled_output()),
        ];

        for (label, failure) in failures {
            let root = temporary_directory(&format!("upload-failure-{label}"));
            let (transaction, source) = upload_fixture(&root);
            let executor = RecordedFileExecutor::new([failure, ok_output()]);
            let error = transaction
                .execute(&executor, &CancellationToken::new())
                .expect_err("failed upload must not succeed");

            assert!(error.to_string().contains(if label == "cancel" {
                "用户取消"
            } else {
                "上传"
            }));
            let commands = executor.commands();
            assert_eq!(commands.len(), 2, "failure {label} should cleanup once");
            assert_eq!(commands[0].args.get(2).map(String::as_str), Some("push"));
            assert_eq!(commands[1].args.get(2).map(String::as_str), Some("shell"));
            // 清理脚本作为单参数直接跟在 shell 后（对齐 C# ShellAsync 的整段
            // 脚本传参）；`sh`/`-c` 形式会被 adb 的 argv 空格拼接拆散。
            assert!(commands[1].args.get(3).is_some_and(|script| script.contains("rm -f --")));
            assert!(!commands[1].args.iter().any(|argument| argument == "sh"));
            let cleanup_script = commands[1].args.last().expect("cleanup script");
            assert!(cleanup_script.contains("rm -f --"));
            assert!(cleanup_script.contains("[ -e ") && cleanup_script.contains("[ -L "));
            assert!(!commands[1]
                .args
                .iter()
                .any(|argument| argument.contains('*')));
            assert_eq!(
                fs::read(source).expect("source should remain"),
                b"upload bytes"
            );
            fs::remove_dir_all(root).expect("temporary directory should be removed");
        }
    }

    #[test]
    fn upload_promote_failure_cleans_temp_and_cleanup_failure_is_not_success() {
        let root = temporary_directory("upload-promote-failure");
        let (transaction, _) = upload_fixture(&root);
        let executor = RecordedFileExecutor::new([ok_output(), failed_output(73), ok_output()]);
        let error = transaction
            .execute(&executor, &CancellationToken::new())
            .expect_err("promote failure must stop the transaction");
        assert!(error.to_string().contains("提交远端文件"));
        let commands = executor.commands();
        assert_eq!(commands.len(), 3);
        // 同上：清理脚本单参数跟在 shell 后，位置 3 即脚本本体。
        assert!(commands[2].args.get(3).is_some_and(|script| script.contains("rm -f --")));
        fs::remove_dir_all(root).expect("temporary directory should be removed");

        let root = temporary_directory("upload-cleanup-failure");
        let (transaction, _) = upload_fixture(&root);
        let executor = RecordedFileExecutor::new([
            external_error("transfer failed"),
            external_error("cleanup failed"),
        ]);
        let error = transaction
            .execute(&executor, &CancellationToken::new())
            .expect_err("cleanup uncertainty must never become success");
        assert!(error.to_string().contains("清理未确认"));
        fs::remove_dir_all(root).expect("temporary directory should be removed");

        let root = temporary_directory("upload-cancel-cleanup-failure");
        let (transaction, _) = upload_fixture(&root);
        let executor =
            RecordedFileExecutor::new([cancelled_output(), external_error("cleanup failed")]);
        let error = transaction
            .execute(&executor, &CancellationToken::new())
            .expect_err("unconfirmed cleanup must override a canceled terminal state");
        assert!(matches!(error, DomainError::ExternalTool(_)));
        assert!(error.to_string().contains("已取消") && error.to_string().contains("清理未确认"));
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn upload_canceled_before_spawn_does_not_claim_or_cleanup_a_remote_temp() {
        let root = temporary_directory("upload-pre-cancel");
        let (transaction, _) = upload_fixture(&root);
        let executor = RecordedFileExecutor::new([]);
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = transaction
            .execute(&executor, &cancellation)
            .expect_err("pre-canceled upload should stop before spawn");
        assert!(matches!(error, DomainError::UserCancelled(_)));
        assert!(executor.commands().is_empty());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn download_success_promotes_sibling_temp_and_preserves_complete_bytes() {
        let root = temporary_directory("download-success");
        let (transaction, destination) = download_fixture(&root);
        let executor = RecordedFileExecutor::new([ok_output()]).with_pull_bytes(b"downloaded");

        transaction
            .execute(&executor, &CancellationToken::new())
            .expect("successful download transaction should complete");

        assert_eq!(
            fs::read(&destination).expect("destination should exist"),
            b"downloaded"
        );
        let commands = executor.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].args.get(2).map(String::as_str), Some("pull"));
        assert!(commands[0]
            .args
            .last()
            .is_some_and(|path| path.contains(".nwflash-download-") && path.ends_with(".partial")));
        assert_eq!(executor.timeouts(), vec![Some(FILE_TRANSFER_TIMEOUT)]);
        let remaining = fs::read_dir(&root)
            .expect("download root should be readable")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![std::ffi::OsString::from("downloaded.bin")]);
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn download_existing_destination_is_rejected_before_any_adb_command() {
        let root = temporary_directory("download-existing");
        let (transaction, destination) = download_fixture(&root);
        fs::write(&destination, b"old").expect("existing destination should be written");
        let executor = RecordedFileExecutor::new([]);

        let error = transaction
            .execute(&executor, &CancellationToken::new())
            .expect_err("existing destination must be rejected");
        assert!(error.to_string().contains("下载目标已存在"));
        assert!(executor.commands().is_empty());
        assert_eq!(
            fs::read(&destination).expect("old destination should remain"),
            b"old"
        );
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn download_transfer_failures_remove_temp_and_leave_no_final_file() {
        let failures = [
            ("nonzero", failed_output(9)),
            ("spawn", external_error("spawn failed")),
            ("timeout", external_error("命令执行超时")),
            ("output", external_error("命令输出超过安全上限")),
            ("read", external_error("读取命令 stdout 输出失败")),
            ("cancel", cancelled_output()),
        ];

        for (label, failure) in failures {
            let root = temporary_directory(&format!("download-failure-{label}"));
            let (transaction, destination) = download_fixture(&root);
            let executor = RecordedFileExecutor::new([failure]);
            let error = transaction
                .execute(&executor, &CancellationToken::new())
                .expect_err("failed download must not succeed");
            assert!(error.to_string().contains(if label == "cancel" {
                "用户取消"
            } else {
                "下载"
            }));
            assert!(!destination.exists());
            assert_eq!(executor.commands().len(), 1);
            let remaining = fs::read_dir(&root)
                .expect("download root should be readable")
                .count();
            assert_eq!(remaining, 0, "temp should be removed for {label}");
            fs::remove_dir_all(root).expect("temporary directory should be removed");
        }
    }

    #[test]
    fn download_promote_conflict_and_missing_temp_fail_closed() {
        let root = temporary_directory("download-conflict");
        let (transaction, destination) = download_fixture(&root);
        let executor = RecordedFileExecutor::new([ok_output()])
            .with_pull_bytes(b"new")
            .with_pull_conflict(destination.clone());
        let error = transaction
            .execute(&executor, &CancellationToken::new())
            .expect_err("a destination appearing before promote must conflict");
        assert!(error.to_string().contains("下载目标已存在"));
        assert_eq!(
            fs::read(&destination).expect("conflicting destination should remain"),
            b"existing"
        );
        assert_eq!(
            fs::read_dir(&root)
                .expect("root should be readable")
                .count(),
            1
        );
        fs::remove_dir_all(root).expect("temporary directory should be removed");

        let root = temporary_directory("download-missing-temp");
        let (transaction, destination) = download_fixture(&root);
        let executor = RecordedFileExecutor::new([ok_output()]).with_pull_remove_temp();
        let error = transaction
            .execute(&executor, &CancellationToken::new())
            .expect_err("a successful pull without a temp file must fail");
        assert!(error.to_string().contains("暂存"));
        assert!(!destination.exists());
        assert_eq!(
            fs::read_dir(&root)
                .expect("root should be readable")
                .count(),
            0
        );
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn download_canceled_before_reservation_has_no_side_effect() {
        let root = temporary_directory("download-pre-cancel");
        let (transaction, destination) = download_fixture(&root);
        let executor = RecordedFileExecutor::new([]);
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = transaction
            .execute(&executor, &cancellation)
            .expect_err("pre-canceled download should stop before reservation");
        assert!(matches!(error, DomainError::UserCancelled(_)));
        assert!(executor.commands().is_empty());
        assert!(!destination.exists());
        assert_eq!(
            fs::read_dir(&root)
                .expect("root should be readable")
                .count(),
            0
        );
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn download_with_missing_parent_fails_before_spawn() {
        let root = temporary_directory("download-missing-parent");
        let destination = root.join("missing").join("downloaded.bin");
        let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");
        let transaction = FileTransaction::Download {
            service,
            serial: "RF8T123".to_string(),
            remote_source: "/sdcard/payload.bin".to_string(),
            local_destination: destination,
            remote_size: None,
        };
        let executor = RecordedFileExecutor::new([]);

        let error = transaction
            .execute(&executor, &CancellationToken::new())
            .expect_err("missing parent must fail before adb");
        assert!(error.to_string().contains("暂存") || error.to_string().contains("目标"));
        assert!(executor.commands().is_empty());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn install_transaction_is_bounded_and_never_creates_file_promote_commands() {
        let root = temporary_directory("install");
        let apk = root.join("manager.apk");
        fs::write(&apk, b"apk").expect("apk fixture should be written");
        let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");
        let command = service
            .build_install_apk_command("RF8T123", &apk)
            .expect("apk command should build");
        let executor = RecordedFileExecutor::new([ok_output()]);
        let transaction = FileTransaction::Install { command };

        transaction
            .execute(&executor, &CancellationToken::new())
            .expect("successful install should complete");
        let commands = executor.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].args.get(2).map(String::as_str), Some("install"));
        assert_eq!(executor.timeouts(), vec![Some(APK_INSTALL_TIMEOUT)]);
        assert_eq!(fs::read(&apk).expect("apk source should remain"), b"apk");
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn install_failures_do_not_run_cleanup_or_modify_the_apk_source() {
        let failures = [
            failed_output(1),
            external_error("spawn failed"),
            external_error("命令执行超时"),
            cancelled_output(),
        ];
        for (index, failure) in failures.into_iter().enumerate() {
            let root = temporary_directory(&format!("install-failure-{index}"));
            let apk = root.join("manager.apk");
            fs::write(&apk, b"apk").expect("apk fixture should be written");
            let service = FileManagerService::with_platform_tools("adb.exe", "fastboot.exe");
            let command = service
                .build_install_apk_command("RF8T123", &apk)
                .expect("apk command should build");
            let executor = RecordedFileExecutor::new([failure]);
            let error = FileTransaction::Install { command }
                .execute(&executor, &CancellationToken::new())
                .expect_err("failed install must not succeed");
            assert!(!executor.commands().is_empty());
            assert_eq!(executor.commands().len(), 1);
            assert!(!executor.commands()[0].args.iter().any(|arg| {
                arg.contains("nwflash-upload") || arg.contains("nwflash-download") || arg == "rm"
            }));
            assert_eq!(fs::read(&apk).expect("apk source should remain"), b"apk");
            assert!(error.to_string().contains("安装") || error.to_string().contains("取消"));
            fs::remove_dir_all(root).expect("temporary directory should be removed");
        }
    }

    #[tokio::test]
    async fn transactional_wrapper_releases_coordinator_after_failure() {
        let root = temporary_directory("coordinator-release");
        let (transaction, _) = upload_fixture(&root);
        let executor = Arc::new(RecordedFileExecutor::new([
            external_error("transfer failed"),
            ok_output(),
        ]));
        let coordinator = OperationCoordinator::default();

        let result = execute_file_transaction_with_executor(
            &coordinator,
            transaction,
            OperationKind::Transferring,
            "测试文件上传",
            executor,
        )
        .await;

        assert!(matches!(result, Err(OperationCoordinatorError::Failed(_))));
        assert!(coordinator.try_acquire_idle().is_ok());
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }
}
