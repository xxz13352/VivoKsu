//! Tauri command for safe-flash plan preview and execution.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use tokio::task;

use nwflash_application::{
    result_to_domain_error, SafeFlashBuildOptions, SafeFlashExecutionRequest,
    SafeFlashExecutionService, SafeFlashPreparationPhase, SafeFlashPreparedSource,
    SafeFlashService, SafeFlashSource,
};
use nwflash_domain::{DomainError, OperationKind, SafeFlashSlotMode};

use crate::commands::device_identity::read_online_ota_identity;
use crate::AppState;

#[derive(Debug)]
pub struct SafeFlashCommandExecutionResultDto {
    pub command_count: usize,
    pub executed_count: usize,
    pub flashed_partition_count: usize,
    pub skipped_partition_count: usize,
}

#[derive(Debug, Clone)]
struct SafeFlashPreparedRequest {
    source: SafeFlashPreparedSource,
    options: SafeFlashBuildOptions,
}

fn prepared_safe_flash_request(
    source: SafeFlashPreparedSource,
    mut options: SafeFlashBuildOptions,
) -> SafeFlashPreparedRequest {
    options.wipe_data_image_path = source.wipe_data_image_path.clone();
    SafeFlashPreparedRequest { source, options }
}

#[derive(Debug, Deserialize)]
pub struct SafeFlashOptionsDto {
    pub is_safe_flash: bool,
    pub is_keep_root: bool,
    pub wipe_data: bool,
    pub slot_mode: SafeFlashSlotMode,
}

#[derive(Debug, Serialize)]
pub struct SafeFlashPreflightDto {
    pub session_id: String,
    pub source_label: String,
    pub partition_count: usize,
    pub safe_partition_count: usize,
    pub has_block_based_content: bool,
    pub requires_confirmation: bool,
}

#[derive(Debug, Serialize)]
pub struct SafeFlashCompletionDto {
    pub flashed_partition_count: usize,
    pub skipped_partition_count: usize,
    pub status: String,
}

#[derive(Debug, Clone)]
struct PreparedSafeFlashSession {
    id: String,
    prepared: SafeFlashPreparedRequest,
}

#[derive(Clone, Default)]
pub struct SafeFlashRuntime {
    state: Arc<Mutex<SafeFlashRuntimeState>>,
}

#[derive(Default)]
struct SafeFlashRuntimeState {
    prepared: Option<PreparedSafeFlashSession>,
    executing: Option<String>,
}

impl SafeFlashRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    fn replace(&self, session: PreparedSafeFlashSession) -> Option<PreparedSafeFlashSession> {
        self.state
            .lock()
            .expect("safe flash runtime lock should not be poisoned")
            .prepared
            .replace(session)
    }

    fn get(&self, id: &str) -> Result<PreparedSafeFlashSession, String> {
        let state = self
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        match state.prepared.as_ref() {
            Some(session) if session.id == id => Ok(session.clone()),
            Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
            None => Err("请先完成线刷预检并确认刷入。".to_string()),
        }
    }

    fn complete(&self, id: &str) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        match state.prepared.as_ref() {
            Some(session) if session.id == id => {
                state.prepared = None;
                if state.executing.as_deref() == Some(id) {
                    state.executing = None;
                }
                Ok(())
            }
            Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
            None => Err("请先完成线刷预检并确认刷入。".to_string()),
        }
    }

    fn cancel(&self, id: &str) -> Result<SafeFlashPreparedRequest, String> {
        let mut state = self
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        if state.executing.as_deref() == Some(id) {
            return Err("线刷已开始执行，不能取消预检或删除临时固件。请使用停止操作。".to_string());
        }
        match state.prepared.as_ref() {
            Some(session) if session.id == id => state
                .prepared
                .take()
                .map(|session| session.prepared)
                .ok_or_else(|| "请先完成线刷预检并确认刷入。".to_string()),
            Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
            None => Err("请先完成线刷预检并确认刷入。".to_string()),
        }
    }

    fn begin_execution(&self, id: &str) -> Result<PreparedSafeFlashSession, String> {
        let mut state = self
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        if state.executing.is_some() {
            return Err("线刷正在执行中。".to_string());
        }
        match state.prepared.as_ref() {
            Some(session) if session.id == id => {
                let session = session.clone();
                state.executing = Some(id.to_string());
                Ok(session)
            }
            Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
            None => Err("请先完成线刷预检并确认刷入。".to_string()),
        }
    }

    fn end_execution(&self, id: &str) {
        let mut state = self
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        if state.executing.as_deref() == Some(id) {
            state.executing = None;
        }
    }
}

fn sanitize_safe_flash_error(error: impl AsRef<str>) -> String {
    let error = error.as_ref();
    if error.contains("读取本地源失败") || error.contains("不支持的来源类型") {
        "读取本地固件失败，请检查所选文件或目录。".to_string()
    } else if error.starts_with("用户取消:") {
        "线刷操作已取消。".to_string()
    } else if error.starts_with("设备不可用:") {
        "设备不可用，请检查连接后重试。".to_string()
    } else if error.starts_with("授权被拒绝:") {
        "线刷授权被拒绝，请重新登录或联系管理员。".to_string()
    } else if error.starts_with("服务端错误:") {
        "在线固件服务暂时不可用，请稍后重试。".to_string()
    } else if error.contains("外部工具执行失败") {
        "线刷执行失败，请检查设备连接后重试。".to_string()
    } else if error.starts_with("文件格式不合法:") {
        "固件格式不合法或不受支持。".to_string()
    } else if error.starts_with("参数错误:") {
        "线刷参数无效，请重新选择固件和选项。".to_string()
    } else if error.starts_with("非法操作:") {
        "当前固件或设备不支持该线刷操作。".to_string()
    } else if error.starts_with("内部错误:") {
        "线刷内部错误，请重试。".to_string()
    } else {
        "线刷操作失败，请重试。".to_string()
    }
}

fn secure_options(serial: String, options: SafeFlashOptionsDto) -> SafeFlashBuildOptions {
    SafeFlashBuildOptions {
        serial,
        is_safe_flash: options.is_safe_flash,
        is_keep_root: options.is_keep_root,
        wipe_data: options.wipe_data,
        wipe_data_image_path: None,
        slot_mode: options.slot_mode,
        current_slot: None,
    }
}

fn active_safe_flash_serial(state: &AppState) -> Result<String, String> {
    state
        .device_runtime
        .active_adb_serial()
        .or_else(|_| state.device_runtime.active_fastboot_serial())
}

fn build_secure_preflight(
    runtime: &SafeFlashRuntime,
    source_label: String,
    prepared: SafeFlashPreparedRequest,
) -> Result<SafeFlashPreflightDto, String> {
    let partition_count = prepared.source.partitions.len();
    let safe_partition_count = match SafeFlashService::new()
        .build_plan(&prepared.source.partitions, prepared.options.clone())
    {
        Ok(plan) => plan.tasks.len(),
        Err(error) => {
            cleanup_safe_flash_staging(
                prepared.source.staging_root.as_deref(),
                SafeFlashStagingOutcome::Success,
            );
            return Err(error.to_string());
        }
    };
    let has_block_based_content = prepared.source.has_block_based_content;
    let id = format!("safe-{}", unique_session_nonce());
    let superseded = runtime.replace(PreparedSafeFlashSession {
        id: id.clone(),
        prepared,
    });
    if let Some(superseded) = superseded {
        cleanup_safe_flash_staging(
            superseded.prepared.source.staging_root.as_deref(),
            SafeFlashStagingOutcome::Success,
        );
    }
    Ok(SafeFlashPreflightDto {
        session_id: id,
        source_label,
        partition_count,
        safe_partition_count,
        has_block_based_content,
        requires_confirmation: true,
    })
}

fn unique_session_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0)
}

enum SafeFlashStagingOutcome {
    Success,
    RecoverableFailure,
}

fn cleanup_safe_flash_staging(
    staging_root: Option<&std::path::Path>,
    outcome: SafeFlashStagingOutcome,
) {
    if matches!(outcome, SafeFlashStagingOutcome::Success) {
        if let Some(staging_root) = staging_root {
            std::fs::remove_dir_all(staging_root).ok();
        }
    }
}

pub(crate) fn session_token(state: &AppState) -> Result<String, String> {
    state
        .session_token
        .read()
        .expect("session token lock should not be poisoned")
        .clone()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "未登录，无法执行线刷。".to_string())
}

fn safe_flash_preparation_progress_sink(
    context: nwflash_application::OperationContext,
    phase_start: f64,
    phase_span: f64,
) -> Arc<nwflash_application::SafeFlashPreparationProgressSink> {
    Arc::new(move |phase, completed_bytes, total_bytes| {
        if total_bytes == 0 {
            return;
        }
        let fraction = (completed_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0);
        let phase_fraction = match phase {
            SafeFlashPreparationPhase::ZipExtraction => fraction,
            SafeFlashPreparationPhase::PayloadStaging => fraction * 0.25,
            SafeFlashPreparationPhase::PayloadExtraction => 0.25 + fraction * 0.75,
        };
        context.report_progress_monotonic((phase_start + phase_span * phase_fraction).min(0.94));
    })
}

#[tauri::command]
pub async fn safe_flash_prepare_online(
    state: State<'_, AppState>,
    options: SafeFlashOptionsDto,
) -> Result<SafeFlashPreflightDto, String> {
    let token = session_token(&state)?;
    let serial = active_safe_flash_serial(&state)?;
    let prepared = Arc::new(Mutex::new(None));
    let prepared_for_operation = prepared.clone();
    let client = state.client.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            "准备 VIVO 线刷",
            move |context, cancellation| async move {
                let (pd, version) = read_online_ota_identity(&serial)
                    .await
                    .map_err(DomainError::DeviceUnavailable)?;
                let build_options = secure_options(serial, options);
                context.report_stage("正在获取在线 OTA 信息");
                let rom = client
                    .resolve_rom(&token, &pd, &version)
                    .await
                    .map_err(|error| DomainError::RemoteApi(error.to_string()))?;
                if cancellation.is_cancelled() {
                    return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
                }
                context.report_stage("正在准备 payload 提取工具");
                let provisioner = nwflash_infrastructure::PayloadDumperProvisioner::new(
                    nwflash_infrastructure::RemoteAssetDownloader::default(),
                    None,
                    None,
                );
                let payload_dumper = provisioner
                    .ensure_installed(&cancellation, None)
                    .await
                    .map_err(|error| {
                        DomainError::ExternalTool(format!("payload 提取工具未就绪：{error}"))
                    })?;
                if cancellation.is_cancelled() {
                    return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
                }
                context.report_stage("正在下载在线 OTA");
                let progress_context = context.clone();
                let download_progress = Arc::new(
                    move |progress: nwflash_infrastructure::OtaDownloadProgress| {
                        if progress.total_bytes > 0 {
                            progress_context.report_progress_monotonic(
                                (progress.downloaded_bytes as f64 / progress.total_bytes as f64)
                                    * 0.70,
                            );
                        }
                    },
                );
                let prepared_source = SafeFlashService::new()
                    .resolve_source_with_cancellation_and_progress(
                        SafeFlashSource::Online {
                            url: rom.url,
                            pd,
                            version,
                            payload_dumper: Some(payload_dumper),
                        },
                        &build_options,
                        &cancellation,
                        Some(download_progress),
                        Some(safe_flash_preparation_progress_sink(
                            context.clone(),
                            0.70,
                            0.24,
                        )),
                    )
                    .await?;
                context.report_stage("正在生成线刷预检");
                context.report_progress_monotonic(0.95);
                *prepared_for_operation
                    .lock()
                    .map_err(|_| DomainError::Internal("线刷预检结果锁不可用。".to_string()))? =
                    Some((
                        rom.name.unwrap_or_else(|| "在线 OTA".to_string()),
                        prepared_safe_flash_request(prepared_source, build_options),
                    ));
                Ok(())
            },
        )
        .await
        .map_err(|error| sanitize_safe_flash_error(error.to_string()))?;
    let (source_label, prepared) = prepared
        .lock()
        .map_err(|_| "线刷预检结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "线刷预检未产生结果。".to_string())?;
    build_secure_preflight(&state.safe_flash_runtime, source_label, prepared)
        .map_err(sanitize_safe_flash_error)
}

#[tauri::command]
pub async fn safe_flash_prepare_local_source(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    options: SafeFlashOptionsDto,
) -> Result<SafeFlashPreflightDto, String> {
    session_token(&state)?;
    let build_options = secure_options(active_safe_flash_serial(&state)?, options);
    let source_path = select_local_safe_flash_source(&app_handle).await?;
    prepare_local_safe_flash_from_path(&state, source_path, build_options).await
}

#[tauri::command]
pub async fn safe_flash_prepare_local_directory(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    options: SafeFlashOptionsDto,
) -> Result<SafeFlashPreflightDto, String> {
    session_token(&state)?;
    let build_options = secure_options(active_safe_flash_serial(&state)?, options);
    let source_path = select_local_safe_flash_directory(&app_handle).await?;
    prepare_local_safe_flash_from_path(&state, source_path, build_options).await
}

async fn prepare_local_safe_flash_from_path(
    state: &AppState,
    source_path: std::path::PathBuf,
    build_options: SafeFlashBuildOptions,
) -> Result<SafeFlashPreflightDto, String> {
    let prepared = Arc::new(Mutex::new(None));
    let prepared_for_operation = prepared.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            "准备 VIVO 线刷",
            move |context, cancellation| async move {
                context.report_stage("正在检查本地固件");
                let source_for_detection = source_path.clone();
                let detection_cancellation = cancellation.clone();
                let payload_source = task::spawn_blocking(move || {
                    if detection_cancellation.is_cancelled() {
                        return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
                    }
                    is_payload_source_path(&source_for_detection)
                        .map_err(DomainError::InvalidOperation)
                })
                .await
                .map_err(|error| {
                    DomainError::Internal(format!("本地固件格式检测调度失败：{error}"))
                })??;
                let prepared_source = if payload_source {
                    context.report_stage("正在准备 payload 提取工具");
                    let provisioner = nwflash_infrastructure::PayloadDumperProvisioner::new(
                        nwflash_infrastructure::RemoteAssetDownloader::default(),
                        None,
                        None,
                    );
                    let executable = provisioner
                        .ensure_installed(&cancellation, None)
                        .await
                        .map_err(|error| {
                            DomainError::ExternalTool(format!("payload 提取工具未就绪：{error}"))
                        })?;
                    if cancellation.is_cancelled() {
                        return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
                    }
                    context.report_stage("正在提取 payload 固件");
                    let payload_options = build_options.clone();
                    let payload_cancellation = cancellation.clone();
                    let preparation_progress =
                        safe_flash_preparation_progress_sink(context.clone(), 0.0, 0.94);
                    task::spawn_blocking(move || {
                        SafeFlashService::new()
                            .resolve_payload_source_with_cancellation_and_progress(
                                &executable,
                                &source_path,
                                &payload_options,
                                &payload_cancellation,
                                Some(&preparation_progress),
                            )
                    })
                    .await
                    .map_err(|error| {
                        DomainError::Internal(format!("payload 固件提取调度失败：{error}"))
                    })??
                } else {
                    context.report_stage("正在解包本地 OTA");
                    let preparation_progress =
                        safe_flash_preparation_progress_sink(context.clone(), 0.0, 0.94);
                    SafeFlashService::new()
                        .resolve_source_with_cancellation_and_progress(
                            SafeFlashSource::LocalPath {
                                path: source_path.to_string_lossy().into_owned(),
                            },
                            &build_options,
                            &cancellation,
                            None,
                            Some(preparation_progress),
                        )
                        .await?
                };
                context.report_stage("正在生成线刷预检");
                context.report_progress_monotonic(0.95);
                *prepared_for_operation
                    .lock()
                    .map_err(|_| DomainError::Internal("线刷预检结果锁不可用。".to_string()))? =
                    Some(prepared_safe_flash_request(prepared_source, build_options));
                Ok(())
            },
        )
        .await
        .map_err(|error| sanitize_safe_flash_error(error.to_string()))?;
    let prepared = prepared
        .lock()
        .map_err(|_| "线刷预检结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "线刷预检未产生结果。".to_string())?;
    build_secure_preflight(&state.safe_flash_runtime, "本地固件".to_string(), prepared)
        .map_err(sanitize_safe_flash_error)
}

fn is_payload_source_path(path: &Path) -> Result<bool, String> {
    let format = nwflash_infrastructure::FirmwareFormatDetector::detect_local(path)
        .map_err(|error| error.to_string())?;
    if format == nwflash_infrastructure::FirmwareFormat::Payload {
        return Ok(true);
    }
    if format == nwflash_infrastructure::FirmwareFormat::Zip {
        return nwflash_infrastructure::FirmwarePackageInspector::contains_payload_bin(path)
            .map_err(|error| error.to_string());
    }
    Ok(false)
}

async fn select_local_safe_flash_source(
    app_handle: &AppHandle,
) -> Result<std::path::PathBuf, String> {
    let (sender, receiver) = oneshot::channel();
    app_handle
        .dialog()
        .file()
        .add_filter("VIVO 固件", &["zip", "bin", "img"])
        .pick_file(move |selected| {
            let _ = sender.send(selected.map(|path| path.into_path()));
        });
    receiver
        .await
        .map_err(|_| "本地固件选择窗口已关闭。".to_string())?
        .transpose()
        .map_err(|_| "无法读取所选本地固件。".to_string())?
        .ok_or_else(|| "用户取消选择本地固件。".to_string())
}

async fn select_local_safe_flash_directory(
    app_handle: &AppHandle,
) -> Result<std::path::PathBuf, String> {
    let (sender, receiver) = oneshot::channel();
    app_handle.dialog().file().pick_folder(move |selected| {
        let _ = sender.send(selected.map(|path| path.into_path()));
    });
    receiver
        .await
        .map_err(|_| "本地固件目录选择窗口已关闭。".to_string())?
        .transpose()
        .map_err(|_| "无法读取所选本地固件目录。".to_string())?
        .ok_or_else(|| "用户取消选择本地固件目录。".to_string())
}

#[tauri::command]
pub async fn safe_flash_execute_prepared(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<SafeFlashCompletionDto, String> {
    session_token(&state)?;
    let session = state.safe_flash_runtime.begin_execution(&session_id)?;
    let result = match execute_prepared_safe_flash(&state, session.prepared).await {
        Ok(result) => result,
        Err(error) => {
            state.safe_flash_runtime.end_execution(&session_id);
            return Err(sanitize_safe_flash_error(error));
        }
    };
    state.safe_flash_runtime.complete(&session_id)?;
    Ok(SafeFlashCompletionDto {
        flashed_partition_count: result.flashed_partition_count,
        skipped_partition_count: result.skipped_partition_count,
        status: format!(
            "已刷入 {} 个分区（完成 {}/{} 个受控步骤）",
            result.flashed_partition_count, result.executed_count, result.command_count
        ),
    })
}

#[tauri::command]
pub fn safe_flash_cancel_prepared(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<(), String> {
    let prepared = state.safe_flash_runtime.cancel(&session_id)?;
    cleanup_safe_flash_staging(
        prepared.source.staging_root.as_deref(),
        SafeFlashStagingOutcome::Success,
    );
    Ok(())
}

async fn execute_prepared_safe_flash(
    state: &AppState,
    prepared: SafeFlashPreparedRequest,
) -> Result<SafeFlashCommandExecutionResultDto, String> {
    let staging_root = prepared.source.staging_root.clone();
    let transition_to_fastbootd = state.device_runtime.active_adb_serial().is_ok();
    let source = prepared.source.clone();
    let options = prepared.options.clone();
    let execution_result = Arc::new(Mutex::new(None));
    let execution_result_for_run = execution_result.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            format!("VIVO线刷({}分区)", source.partitions.len()),
            move |context, cancellation| {
                let source = source.clone();
                let options = options.clone();
                let execution_result_for_run = execution_result_for_run.clone();
                async move {
                    let stage_context = context.clone();
                    let progress_context = context;
                    let execution = task::spawn_blocking(move || {
                        SafeFlashExecutionService::system().execute(
                            SafeFlashExecutionRequest {
                                source: &source,
                                options: &options,
                                transition_to_fastbootd,
                                expected_serial: None,
                            },
                            || cancellation.is_cancelled(),
                            |stage| stage_context.report_stage(stage),
                            |progress| progress_context.report_progress_monotonic(progress),
                        )
                    })
                    .await
                    .map_err(|error| {
                        DomainError::Internal(format!("线刷执行调度失败：{error}"))
                    })??;
                    *execution_result_for_run
                        .lock()
                        .expect("safe flash execution result lock should not be poisoned") =
                        Some(execution);
                    Ok(())
                }
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    cleanup_safe_flash_staging(staging_root.as_deref(), SafeFlashStagingOutcome::Success);

    let result = execution_result
        .lock()
        .expect("safe flash execution result lock should not be poisoned")
        .take()
        .ok_or_else(|| "线刷执行未返回结果。".to_string())?;
    Ok(SafeFlashCommandExecutionResultDto {
        command_count: result.command_count,
        executed_count: result.executed_command_count,
        flashed_partition_count: result.flashed_partition_count,
        skipped_partition_count: result.skipped_partition_count,
    })
}

fn needs_fastbootd_transition(state: nwflash_domain::DeviceConnectionState) -> bool {
    state == nwflash_domain::DeviceConnectionState::AdbConnected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, File},
        time::{SystemTime, UNIX_EPOCH},
    };
    use zip4::{write::SimpleFileOptions, ZipWriter};

    #[test]
    fn secure_preflight_dto_never_serializes_device_or_file_execution_secrets() {
        let dto = SafeFlashPreflightDto {
            session_id: "safe-opaque".to_string(),
            source_label: "在线 OTA".to_string(),
            partition_count: 3,
            safe_partition_count: 2,
            has_block_based_content: true,
            requires_confirmation: true,
        };

        let value = serde_json::to_string(&dto).expect("DTO should serialize");
        assert!(!value.contains("serial"));
        assert!(!value.contains("url"));
        assert!(!value.contains("path"));
        assert!(!value.contains("command"));
        assert!(value.contains("has_block_based_content"));
    }

    #[test]
    fn safe_flash_runtime_keeps_preflight_until_the_matching_execution_succeeds() {
        let runtime = SafeFlashRuntime::new();
        runtime.replace(PreparedSafeFlashSession {
            id: "safe-one".to_string(),
            prepared: SafeFlashPreparedRequest {
                source: SafeFlashPreparedSource {
                    staging_root: None,
                    partitions: Vec::new(),
                    wipe_data_image_path: None,
                    has_block_based_content: false,
                },
                options: SafeFlashBuildOptions {
                    serial: "internal-device".to_string(),
                    is_safe_flash: false,
                    is_keep_root: false,
                    wipe_data: false,
                    wipe_data_image_path: None,
                    slot_mode: SafeFlashSlotMode::CurrentSlot,
                    current_slot: None,
                },
            },
        });

        assert!(runtime.get("safe-other").is_err());
        assert!(runtime.get("safe-one").is_ok());
        assert!(runtime.get("safe-one").is_ok());
        assert!(runtime.complete("safe-other").is_err());
        assert!(runtime.get("safe-one").is_ok());
        runtime
            .complete("safe-one")
            .expect("successful execution should consume its preflight");
        assert!(runtime.get("safe-one").is_err());
    }

    #[test]
    fn prepared_cancel_is_rejected_while_execution_owns_the_staging() {
        let runtime = SafeFlashRuntime::new();
        runtime.replace(PreparedSafeFlashSession {
            id: "safe-running".to_string(),
            prepared: SafeFlashPreparedRequest {
                source: SafeFlashPreparedSource {
                    staging_root: None,
                    partitions: Vec::new(),
                    wipe_data_image_path: None,
                    has_block_based_content: false,
                },
                options: SafeFlashBuildOptions {
                    serial: "internal-device".to_string(),
                    is_safe_flash: true,
                    is_keep_root: false,
                    wipe_data: false,
                    wipe_data_image_path: None,
                    slot_mode: SafeFlashSlotMode::CurrentSlot,
                    current_slot: None,
                },
            },
        });

        runtime
            .begin_execution("safe-running")
            .expect("execution should claim the prepared session");

        assert!(runtime.cancel("safe-running").is_err());
        assert!(runtime.get("safe-running").is_ok());
    }

    #[test]
    fn execution_claim_is_an_atomic_transition_against_prepared_cancellation() {
        let runtime = SafeFlashRuntime::new();
        runtime.replace(PreparedSafeFlashSession {
            id: "safe-atomic".to_string(),
            prepared: SafeFlashPreparedRequest {
                source: SafeFlashPreparedSource {
                    staging_root: None,
                    partitions: Vec::new(),
                    wipe_data_image_path: None,
                    has_block_based_content: false,
                },
                options: SafeFlashBuildOptions {
                    serial: "internal-device".to_string(),
                    is_safe_flash: true,
                    is_keep_root: false,
                    wipe_data: false,
                    wipe_data_image_path: None,
                    slot_mode: SafeFlashSlotMode::CurrentSlot,
                    current_slot: None,
                },
            },
        });

        runtime
            .begin_execution("safe-atomic")
            .expect("the execution claim should own the prepared session atomically");
        assert!(runtime.cancel("safe-atomic").is_err());
        assert!(runtime.get("safe-atomic").is_ok());
    }

    #[test]
    fn local_safe_flash_error_does_not_expose_selected_path() {
        let error = sanitize_safe_flash_error(
            "读取本地源失败：C:\\Users\\17254\\Private\\firmware.zip（拒绝访问）",
        );

        assert!(!error.contains("C:\\Users\\17254\\Private"));
        assert_eq!(error, "读取本地固件失败，请检查所选文件或目录。");
    }

    #[test]
    fn safe_flash_error_boundary_rejects_untrusted_tool_output_without_a_path() {
        let error = sanitize_safe_flash_error(
            "外部工具执行失败: fastboot -s SERIAL-SECRET failed token=private",
        );

        assert_eq!(error, "线刷执行失败，请检查设备连接后重试。");
        assert!(!error.contains("SERIAL-SECRET"));
        assert!(!error.contains("private"));
        assert!(!error.contains("fastboot"));
    }

    #[test]
    fn safe_flash_error_boundary_is_closed_for_unrecognized_sensitive_text() {
        let error = sanitize_safe_flash_error(
            "unexpected SERIAL-SECRET token=private https://rom.invalid/ota.zip",
        );

        assert_eq!(error, "线刷操作失败，请重试。");
        assert!(!error.contains("SERIAL-SECRET"));
        assert!(!error.contains("private"));
        assert!(!error.contains("rom.invalid"));
    }

    #[test]
    fn prepared_safe_flash_binds_only_its_generated_wipe_image() {
        let source = SafeFlashPreparedSource {
            staging_root: None,
            partitions: Vec::new(),
            wipe_data_image_path: Some("C:\\internal\\wipe-data.img".to_string()),
            has_block_based_content: false,
        };
        let options = SafeFlashBuildOptions {
            serial: "internal-device".to_string(),
            is_safe_flash: false,
            is_keep_root: false,
            wipe_data: true,
            wipe_data_image_path: None,
            slot_mode: SafeFlashSlotMode::CurrentSlot,
            current_slot: None,
        };

        let prepared = prepared_safe_flash_request(source, options);

        assert_eq!(
            prepared.options.wipe_data_image_path.as_deref(),
            Some("C:\\internal\\wipe-data.img")
        );
    }

    #[test]
    fn replacing_a_preflight_releases_the_superseded_safe_flash_staging() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let superseded_staging =
            std::env::temp_dir().join(format!("nwflash-safe-preflight-{nonce}"));
        fs::create_dir_all(&superseded_staging).expect("staging should be created");
        fs::write(superseded_staging.join("ota.zip"), b"staged")
            .expect("staging file should be created");
        let runtime = SafeFlashRuntime::new();
        let prepared = |staging_root| SafeFlashPreparedRequest {
            source: SafeFlashPreparedSource {
                staging_root,
                partitions: vec![nwflash_application::SafeFlashPartitionSource {
                    partition_name: "userdata".to_string(),
                    image_path: "C:\\internal\\userdata.img".to_string(),
                    has_slot: false,
                }],
                wipe_data_image_path: None,
                has_block_based_content: false,
            },
            options: SafeFlashBuildOptions {
                serial: "internal-device".to_string(),
                is_safe_flash: true,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        };

        build_secure_preflight(
            &runtime,
            "旧固件".to_string(),
            prepared(Some(superseded_staging.clone())),
        )
        .expect("first preflight should be created");
        build_secure_preflight(&runtime, "新固件".to_string(), prepared(None))
            .expect("replacement preflight should be created");

        assert!(!superseded_staging.exists());
    }

    #[test]
    fn rejected_preflight_releases_unpublished_safe_flash_staging() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let staging = std::env::temp_dir().join(format!("nwflash-safe-preflight-rejected-{nonce}"));
        fs::create_dir_all(&staging).expect("staging should be created");
        fs::write(staging.join("ota.zip"), b"staged").expect("staging file should be created");
        let runtime = SafeFlashRuntime::new();
        let prepared = SafeFlashPreparedRequest {
            source: SafeFlashPreparedSource {
                staging_root: Some(staging.clone()),
                partitions: Vec::new(),
                wipe_data_image_path: None,
                has_block_based_content: false,
            },
            options: SafeFlashBuildOptions {
                serial: "internal-device".to_string(),
                is_safe_flash: true,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        };

        assert!(build_secure_preflight(&runtime, "空固件".to_string(), prepared).is_err());
        assert!(!staging.exists());
    }

    #[test]
    fn payload_format_detection_routes_raw_payloads_to_the_controlled_extractor() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let source = std::env::temp_dir().join(format!("nwflash-safe-payload-{nonce}.bin"));
        fs::write(&source, b"CrAU").expect("payload fixture should be written");

        assert!(is_payload_source_path(&source).expect("raw payload should be detectable"));

        fs::remove_file(source).expect("payload fixture should be removed");
    }

    #[test]
    fn payload_format_detection_routes_payload_zip_to_the_controlled_extractor() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let source = std::env::temp_dir().join(format!("nwflash-safe-payload-{nonce}.zip"));
        let mut archive = ZipWriter::new(File::create(&source).expect("zip fixture should open"));
        archive
            .start_file("firmware/payload.bin", SimpleFileOptions::default())
            .expect("payload entry should open");
        std::io::Write::write_all(&mut archive, b"CrAU").expect("payload entry should be written");
        archive.finish().expect("zip fixture should finish");

        assert!(is_payload_source_path(&source).expect("payload ZIP should be detectable"));

        fs::remove_file(source).expect("payload ZIP fixture should be removed");
    }

    #[test]
    fn safe_flash_only_transitions_adb_devices_to_fastbootd() {
        assert!(needs_fastbootd_transition(
            nwflash_domain::DeviceConnectionState::AdbConnected
        ));
        assert!(!needs_fastbootd_transition(
            nwflash_domain::DeviceConnectionState::FastbootConnected
        ));
        assert!(!needs_fastbootd_transition(
            nwflash_domain::DeviceConnectionState::Disconnected
        ));
    }

    #[test]
    fn successful_execution_cleans_only_the_owned_safe_flash_staging_root() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("nwflash-safe-flash-cleanup-{nonce}"));
        let owned = root.join("owned");
        let user_file = root.join("user-image.img");
        fs::create_dir_all(&owned).expect("owned staging should be created");
        fs::write(owned.join("staged.img"), b"staged").expect("staged image should be written");
        fs::write(&user_file, b"user").expect("user image should be written");

        cleanup_safe_flash_staging(Some(&owned), SafeFlashStagingOutcome::Success);

        assert!(!owned.exists());
        assert_eq!(
            fs::read(&user_file).expect("user image must remain"),
            b"user"
        );
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn failed_execution_keeps_owned_safe_flash_staging_recoverable() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let owned = std::env::temp_dir().join(format!("nwflash-safe-flash-retain-{nonce}"));
        fs::create_dir_all(&owned).expect("owned staging should be created");
        fs::write(owned.join("staged.img"), b"staged").expect("staged image should be written");

        cleanup_safe_flash_staging(Some(&owned), SafeFlashStagingOutcome::RecoverableFailure);

        assert!(owned.join("staged.img").exists());
        fs::remove_dir_all(owned).expect("fixture directory should be removed");
    }
}
