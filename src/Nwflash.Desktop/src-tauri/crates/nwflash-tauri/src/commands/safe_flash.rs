//! Tauri command for safe-flash plan preview and execution.

use std::{
    path::{Path, PathBuf},
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
use nwflash_infrastructure::SecretToken;

use crate::commands::device_identity::read_online_ota_identity;
use crate::{
    commands::device::DeviceRuntime,
    session_capabilities::{SessionCapabilityLease, SessionCapabilityScope},
    AppState,
};

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

#[derive(Debug, Clone)]
struct PreparedSafeFlashEntry {
    epoch: u64,
    session: PreparedSafeFlashSession,
}

#[derive(Clone)]
pub struct SafeFlashRuntime {
    scope: Arc<SessionCapabilityScope>,
    state: Arc<Mutex<SafeFlashRuntimeState>>,
}

#[derive(Default)]
struct SafeFlashRuntimeState {
    prepared: Option<PreparedSafeFlashEntry>,
    executing: Option<(u64, String)>,
}

impl SafeFlashRuntime {
    pub fn new() -> Self {
        Self::with_scope(Arc::new(SessionCapabilityScope::new()))
    }

    pub(crate) fn with_scope(scope: Arc<SessionCapabilityScope>) -> Self {
        Self {
            scope,
            state: Arc::new(Mutex::new(SafeFlashRuntimeState::default())),
        }
    }

    fn capture_lease(&self) -> Result<SessionCapabilityLease, String> {
        self.scope
            .capture()
            .map_err(|_| "当前会话已失效，请重新完成线刷预检。".to_string())
    }

    #[cfg(test)]
    fn replace(
        &self,
        session: PreparedSafeFlashSession,
    ) -> Result<Option<PreparedSafeFlashSession>, String> {
        let lease = self.capture_lease()?;
        self.replace_with_lease(lease, session)
    }

    fn replace_with_lease(
        &self,
        lease: SessionCapabilityLease,
        session: PreparedSafeFlashSession,
    ) -> Result<Option<PreparedSafeFlashSession>, String> {
        self.scope
            .commit(lease, || {
                self.state
                    .lock()
                    .expect("safe flash runtime lock should not be poisoned")
                    .prepared
                    .replace(PreparedSafeFlashEntry {
                        epoch: lease.epoch,
                        session,
                    })
                    .map(|entry| entry.session)
            })
            .map_err(|_| "当前会话已失效，请重新完成线刷预检。".to_string())
    }

    #[cfg(test)]
    fn get(&self, id: &str) -> Result<PreparedSafeFlashSession, String> {
        let lease = self.capture_lease()?;
        self.scope
            .commit(lease, || {
                let state = self
                    .state
                    .lock()
                    .expect("safe flash runtime lock should not be poisoned");
                match state.prepared.as_ref() {
                    Some(entry) if entry.epoch == lease.epoch && entry.session.id == id => {
                        Ok(entry.session.clone())
                    }
                    Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
                    None => Err("请先完成线刷预检并确认刷入。".to_string()),
                }
            })
            .map_err(|_| "当前会话已失效，请重新完成线刷预检。".to_string())?
    }

    fn complete(&self, id: &str) -> Result<(), String> {
        let lease = self.capture_lease()?;
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("safe flash runtime lock should not be poisoned");
                match state.prepared.as_ref() {
                    Some(entry) if entry.epoch == lease.epoch && entry.session.id == id => {
                        state.prepared = None;
                        if state
                            .executing
                            .as_ref()
                            .is_some_and(|(epoch, executing_id)| {
                                *epoch == lease.epoch && executing_id == id
                            })
                        {
                            state.executing = None;
                        }
                        Ok(())
                    }
                    Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
                    None => Err("请先完成线刷预检并确认刷入。".to_string()),
                }
            })
            .map_err(|_| "当前会话已失效，请重新完成线刷预检。".to_string())?
    }

    fn cancel(&self, id: &str) -> Result<SafeFlashPreparedRequest, String> {
        let lease = self.capture_lease()?;
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("safe flash runtime lock should not be poisoned");
                if state
                    .executing
                    .as_ref()
                    .is_some_and(|(epoch, executing_id)| {
                        *epoch == lease.epoch && executing_id == id
                    })
                {
                    return Err(
                        "线刷已开始执行，不能取消预检或删除临时固件。请使用停止操作。".to_string(),
                    );
                }
                match state.prepared.as_ref() {
                    Some(entry) if entry.epoch == lease.epoch && entry.session.id == id => state
                        .prepared
                        .take()
                        .map(|entry| entry.session.prepared)
                        .ok_or_else(|| "请先完成线刷预检并确认刷入。".to_string()),
                    Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
                    None => Err("请先完成线刷预检并确认刷入。".to_string()),
                }
            })
            .map_err(|_| "当前会话已失效，请重新完成线刷预检。".to_string())?
    }

    fn begin_execution(&self, id: &str) -> Result<PreparedSafeFlashSession, String> {
        let lease = self.capture_lease()?;
        self.scope
            .commit(lease, || {
                let mut state = self
                    .state
                    .lock()
                    .expect("safe flash runtime lock should not be poisoned");
                if state.executing.is_some() {
                    return Err("线刷正在执行中。".to_string());
                }
                match state.prepared.as_ref() {
                    Some(entry) if entry.epoch == lease.epoch && entry.session.id == id => {
                        let session = entry.session.clone();
                        state.executing = Some((lease.epoch, id.to_string()));
                        Ok(session)
                    }
                    Some(_) => Err("线刷预检已失效，请重新预检。".to_string()),
                    None => Err("请先完成线刷预检并确认刷入。".to_string()),
                }
            })
            .map_err(|_| "当前会话已失效，请重新完成线刷预检。".to_string())?
    }

    fn end_execution(&self, id: &str) {
        let Ok(lease) = self.scope.capture() else {
            return;
        };
        let _ = self.scope.commit(lease, || {
            let mut state = self
                .state
                .lock()
                .expect("safe flash runtime lock should not be poisoned");
            if state
                .executing
                .as_ref()
                .is_some_and(|(epoch, executing_id)| *epoch == lease.epoch && executing_id == id)
            {
                state.executing = None;
            }
        });
    }

    pub(crate) fn clear_owned(&self) -> Vec<PathBuf> {
        let mut state = self
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        state.executing = None;
        state
            .prepared
            .take()
            .and_then(|entry| entry.session.prepared.source.staging_root)
            .into_iter()
            .collect()
    }
}

impl Default for SafeFlashRuntime {
    fn default() -> Self {
        Self::new()
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
    } else if error.starts_with("服务端错误:")
        && (error.contains("服务端返回 404") || error.contains("record not found"))
    {
        "在线固件库没有找到匹配的设备版本，请确认设备型号和系统版本。".to_string()
    } else if error.starts_with("服务端错误:")
        && (error.contains("服务端返回 401") || error.contains("请先登录"))
    {
        "在线固件服务登录状态已失效，请重新登录。".to_string()
    } else if error.starts_with("服务端错误:")
        && (error.contains("服务端返回 402") || error.contains("INSUFFICIENT_CREDITS"))
    {
        "在线固件服务余额不足，请联系管理员。".to_string()
    } else if error.starts_with("服务端错误:") && error.contains("服务端返回 403") {
        "在线固件服务拒绝了当前账号，请联系管理员。".to_string()
    } else if error.starts_with("服务端错误:")
        && (error.contains("服务端返回 502") || error.contains("上游"))
    {
        "在线固件上游服务连接失败，请稍后重试。".to_string()
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

fn active_safe_flash_serial(device_runtime: &DeviceRuntime) -> Result<String, String> {
    device_runtime
        .active_adb_serial()
        .or_else(|_| device_runtime.active_fastboot_serial())
}

#[cfg(test)]
fn build_secure_preflight(
    runtime: &SafeFlashRuntime,
    source_label: String,
    prepared: SafeFlashPreparedRequest,
) -> Result<SafeFlashPreflightDto, String> {
    let lease = match runtime.capture_lease() {
        Ok(lease) => lease,
        Err(error) => {
            cleanup_safe_flash_staging(
                prepared.source.staging_root.as_deref(),
                SafeFlashStagingOutcome::Success,
            );
            return Err(error);
        }
    };
    build_secure_preflight_with_lease(runtime, lease, source_label, prepared)
}

fn build_secure_preflight_with_lease(
    runtime: &SafeFlashRuntime,
    lease: SessionCapabilityLease,
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
    let candidate_staging_root = prepared.source.staging_root.clone();
    let superseded = match runtime.replace_with_lease(
        lease,
        PreparedSafeFlashSession {
            id: id.clone(),
            prepared,
        },
    ) {
        Ok(superseded) => superseded,
        Err(error) => {
            cleanup_safe_flash_staging(
                candidate_staging_root.as_deref(),
                SafeFlashStagingOutcome::Success,
            );
            return Err(error);
        }
    };
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
    #[cfg(test)]
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

pub(crate) fn session_token(state: &AppState) -> Result<SecretToken, String> {
    state
        .session_token
        .read()
        .expect("session token lock should not be poisoned")
        .as_ref()
        .filter(|token| !token.is_empty())
        .map(SecretToken::request_scope)
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
    let lease = state.safe_flash_runtime.capture_lease()?;
    let prepared = Arc::new(Mutex::new(None));
    let prepared_for_operation = prepared.clone();
    let client = state.client.clone();
    let device_runtime = state.device_runtime.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            "准备 VIVO 线刷",
            move |context, cancellation| async move {
                let serial = active_safe_flash_serial(&device_runtime)
                    .map_err(DomainError::DeviceUnavailable)?;
                let (pd, version) = read_online_ota_identity(&serial)
                    .await
                    .map_err(DomainError::DeviceUnavailable)?;
                let build_options = secure_options(serial, options);
                context.report_stage("正在获取在线 OTA 信息");
                let rom = client
                    .resolve_rom(token.as_str(), &pd, &version)
                    .await
                    .map_err(|error| {
                        DomainError::RemoteApi(format!(
                            "查询在线固件失败（PD: {pd}，版本: {version}）：{}",
                            error.user_message()
                        ))
                    })?;
                if cancellation.is_cancelled() {
                    return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
                }
                context.report_stage("正在准备 payload 提取工具");
                let provisioner = nwflash_infrastructure::PayloadDumperProvisioner::bundled(
                    nwflash_windows::bundled_resource_root(),
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
    build_secure_preflight_with_lease(&state.safe_flash_runtime, lease, source_label, prepared)
        .map_err(sanitize_safe_flash_error)
}

#[tauri::command]
pub async fn safe_flash_prepare_local_source(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    options: SafeFlashOptionsDto,
) -> Result<SafeFlashPreflightDto, String> {
    session_token(&state)?;
    let lease = state.safe_flash_runtime.capture_lease()?;
    let source_path = select_local_safe_flash_source(&app_handle).await?;
    prepare_local_safe_flash_from_path(&state, lease, source_path, options).await
}

#[tauri::command]
pub async fn safe_flash_prepare_local_directory(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    options: SafeFlashOptionsDto,
) -> Result<SafeFlashPreflightDto, String> {
    session_token(&state)?;
    let lease = state.safe_flash_runtime.capture_lease()?;
    let source_path = select_local_safe_flash_directory(&app_handle).await?;
    prepare_local_safe_flash_from_path(&state, lease, source_path, options).await
}

async fn prepare_local_safe_flash_from_path(
    state: &AppState,
    lease: SessionCapabilityLease,
    source_path: std::path::PathBuf,
    options: SafeFlashOptionsDto,
) -> Result<SafeFlashPreflightDto, String> {
    let prepared = Arc::new(Mutex::new(None));
    let prepared_for_operation = prepared.clone();
    let device_runtime = state.device_runtime.clone();
    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            "准备 VIVO 线刷",
            move |context, cancellation| async move {
                let serial = active_safe_flash_serial(&device_runtime)
                    .map_err(DomainError::DeviceUnavailable)?;
                let build_options = secure_options(serial, options);
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
                    let provisioner = nwflash_infrastructure::PayloadDumperProvisioner::bundled(
                        nwflash_windows::bundled_resource_root(),
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
    build_secure_preflight_with_lease(
        &state.safe_flash_runtime,
        lease,
        "本地固件".to_string(),
        prepared,
    )
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
    safe_flash_execute_prepared_inner(&state, session_id, SafeFlashExecutionService::system()).await
}

async fn safe_flash_execute_prepared_inner(
    state: &AppState,
    session_id: String,
    execution_service: SafeFlashExecutionService,
) -> Result<SafeFlashCompletionDto, String> {
    let result = execute_session_bound_safe_flash(state, session_id, execution_service)
        .await
        .map_err(sanitize_safe_flash_error)?;
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

#[cfg(test)]
async fn execute_prepared_safe_flash(
    state: &AppState,
    prepared: SafeFlashPreparedRequest,
    execution_service: SafeFlashExecutionService,
) -> Result<SafeFlashCommandExecutionResultDto, String> {
    let device_runtime = state.device_runtime.clone();
    let staging_root = prepared.source.staging_root.clone();
    let execution_result = Arc::new(Mutex::new(None));
    let execution_result_for_run = execution_result.clone();
    let partition_count = prepared.source.partitions.len();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            format!("VIVO线刷({partition_count}分区)"),
            move |context, cancellation| {
                let execution_result_for_run = execution_result_for_run.clone();
                let device_runtime = device_runtime.clone();
                async move {
                    let execution = execute_safe_flash_request(
                        device_runtime,
                        prepared,
                        execution_service,
                        context,
                        cancellation,
                    )
                    .await?;
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

async fn execute_session_bound_safe_flash(
    state: &AppState,
    session_id: String,
    execution_service: SafeFlashExecutionService,
) -> Result<SafeFlashCommandExecutionResultDto, String> {
    let device_runtime = state.device_runtime.clone();
    let safe_flash_runtime = state.safe_flash_runtime.clone();
    let execution_result = Arc::new(Mutex::new(None));
    let execution_result_for_run = execution_result.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            "VIVO线刷",
            move |context, cancellation| {
                let device_runtime = device_runtime.clone();
                let safe_flash_runtime = safe_flash_runtime.clone();
                let session_id = session_id.clone();
                let execution_result_for_run = execution_result_for_run.clone();
                async move {
                    let _token =
                        session_token(state).map_err(DomainError::AuthorizationDenied)?;
                    let session = safe_flash_runtime
                        .begin_execution(&session_id)
                        .map_err(DomainError::InvalidOperation)?;
                    let staging_root = session.prepared.source.staging_root.clone();
                    let execution = match execute_safe_flash_request(
                        device_runtime,
                        session.prepared,
                        execution_service,
                        context,
                        cancellation,
                    )
                    .await
                    {
                        Ok(execution) => execution,
                        Err(error) => {
                            safe_flash_runtime.end_execution(&session_id);
                            return Err(error);
                        }
                    };
                    if let Err(error) = safe_flash_runtime.complete(&session_id) {
                        safe_flash_runtime.end_execution(&session_id);
                        return Err(DomainError::InvalidOperation(error));
                    }
                    cleanup_safe_flash_staging(
                        staging_root.as_deref(),
                        SafeFlashStagingOutcome::Success,
                    );
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

async fn execute_safe_flash_request(
    device_runtime: DeviceRuntime,
    prepared: SafeFlashPreparedRequest,
    execution_service: SafeFlashExecutionService,
    context: nwflash_application::OperationContext,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<nwflash_application::SafeFlashExecutionResult, DomainError> {
    let (serial, transition_to_fastbootd) = match device_runtime.active_adb_serial() {
        Ok(serial) => (serial, true),
        Err(_) => (
            device_runtime
                .active_fastboot_serial()
                .map_err(DomainError::DeviceUnavailable)?,
            false,
        ),
    };
    let stage_context = context.clone();
    let progress_context = context;
    task::spawn_blocking(move || {
        execution_service.execute(
            SafeFlashExecutionRequest {
                source: &prepared.source,
                options: &prepared.options,
                serial: &serial,
                transition_to_fastbootd,
            },
            || cancellation.is_cancelled(),
            |stage| stage_context.report_stage(stage),
            |progress| progress_context.report_progress_monotonic(progress),
        )
    })
    .await
    .map_err(|error| DomainError::Internal(format!("线刷执行调度失败：{error}")))?
}

#[cfg(test)]
fn needs_fastbootd_transition(state: nwflash_domain::DeviceConnectionState) -> bool {
    state == nwflash_domain::DeviceConnectionState::AdbConnected
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use nwflash_application::{
        OperationAuthorization, OperationCoordinator, OperationCoordinatorError,
        OperationPermissionGate,
    };
    use nwflash_domain::{DeviceConnectionState, DeviceRefreshMode, DeviceSnapshot};
    use nwflash_windows::process::{CancellableProcessExecutor, ProcessCommand, ProcessOutput};
    use std::{
        collections::VecDeque,
        fs::{self, File},
        sync::{Arc, Mutex},
        time::{SystemTime, UNIX_EPOCH},
    };
    use zip4::{write::SimpleFileOptions, ZipWriter};

    struct BlockingSafeFlashAuthorization {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    impl OperationPermissionGate for BlockingSafeFlashAuthorization {
        fn authorize(
            &self,
            _operation: OperationKind,
            _title: String,
        ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
            let entered = self.entered.clone();
            let release = self.release.clone();
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Ok(OperationAuthorization::allow())
            })
        }
    }

    struct BlockingSafeFlashDecision {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        authorization: OperationAuthorization,
    }

    impl OperationPermissionGate for BlockingSafeFlashDecision {
        fn authorize(
            &self,
            _operation: OperationKind,
            _title: String,
        ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
            let entered = self.entered.clone();
            let release = self.release.clone();
            let authorization = self.authorization.clone();
            Box::pin(async move {
                entered.notify_one();
                release.notified().await;
                Ok(authorization)
            })
        }
    }

    #[derive(Clone)]
    struct RecordedSafeFlashExecutor {
        commands: Arc<Mutex<Vec<ProcessCommand>>>,
        outputs: Arc<Mutex<VecDeque<Result<ProcessOutput, DomainError>>>>,
    }

    impl RecordedSafeFlashExecutor {
        fn new(outputs: impl IntoIterator<Item = Result<ProcessOutput, DomainError>>) -> Self {
            Self {
                commands: Arc::new(Mutex::new(Vec::new())),
                outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
            }
        }

        fn commands(&self) -> Vec<ProcessCommand> {
            self.commands
                .lock()
                .expect("recorded commands lock should not be poisoned")
                .clone()
        }
    }

    impl CancellableProcessExecutor for RecordedSafeFlashExecutor {
        fn run(
            &self,
            command: ProcessCommand,
            should_cancel: &mut dyn FnMut() -> bool,
        ) -> Result<ProcessOutput, DomainError> {
            if should_cancel() {
                return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
            }
            self.commands
                .lock()
                .expect("recorded commands lock should not be poisoned")
                .push(command);
            self.outputs
                .lock()
                .expect("recorded outputs lock should not be poisoned")
                .pop_front()
                .expect("test must provide one output for every command")
        }
    }

    fn successful_process_output(stdout: &str) -> Result<ProcessOutput, DomainError> {
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    fn active_safe_flash_runtime() -> SafeFlashRuntime {
        let scope = Arc::new(SessionCapabilityScope::new());
        scope.activate();
        SafeFlashRuntime::with_scope(scope)
    }

    fn prepared_safe_flash_session(id: &str) -> PreparedSafeFlashSession {
        PreparedSafeFlashSession {
            id: id.to_string(),
            prepared: SafeFlashPreparedRequest {
                source: SafeFlashPreparedSource {
                    staging_root: None,
                    partitions: vec![nwflash_application::SafeFlashPartitionSource {
                        partition_name: "boot".to_string(),
                        image_path: r"C:\test-only\boot.img".to_string(),
                        has_slot: false,
                    }],
                    wipe_data_image_path: None,
                    has_block_based_content: false,
                },
                options: SafeFlashBuildOptions {
                    serial: "PREFLIGHT-DEVICE".to_string(),
                    is_safe_flash: false,
                    is_keep_root: false,
                    wipe_data: false,
                    wipe_data_image_path: None,
                    slot_mode: SafeFlashSlotMode::CurrentSlot,
                    current_slot: None,
                },
            },
        }
    }

    fn safe_flash_is_prepared_and_not_executing(runtime: &SafeFlashRuntime, id: &str) -> bool {
        let state = runtime
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        state
            .prepared
            .as_ref()
            .is_some_and(|entry| entry.session.id == id)
            && state.executing.is_none()
    }

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
        let runtime = active_safe_flash_runtime();
        runtime
            .replace(PreparedSafeFlashSession {
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
            })
            .expect("current session should publish the prepared Safe Flash entry");

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn denied_safe_flash_admission_leaves_the_session_prepared_and_cancelable() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingSafeFlashDecision {
                entered: entered.clone(),
                release: release.clone(),
                authorization: OperationAuthorization::deny("test denial"),
            })),
            None,
            None,
            None,
        );
        state.session_capabilities.activate();
        *state
            .session_token
            .write()
            .expect("session token lock should not be poisoned") =
            Some(SecretToken::new("test-token".to_string()));
        let session_id = "safe-denied-admission".to_string();
        state
            .safe_flash_runtime
            .replace(prepared_safe_flash_session(&session_id))
            .expect("current session should publish the prepared Safe Flash entry");
        let executor = RecordedSafeFlashExecutor::new(
            Vec::<Result<ProcessOutput, DomainError>>::new(),
        );
        let execution_service = SafeFlashExecutionService::new(Arc::new(executor.clone()));

        let execution = safe_flash_execute_prepared_inner(
            &state,
            session_id.clone(),
            execution_service,
        );
        let observe_pending_authorization = async {
            entered.notified().await;
            assert!(safe_flash_is_prepared_and_not_executing(
                &state.safe_flash_runtime,
                &session_id
            ));
            assert!(executor.commands().is_empty());
            assert!(matches!(
                state.operation_coordinator.try_acquire_idle(),
                Err(OperationCoordinatorError::InProgress)
            ));
            release.notify_one();
        };
        let (denied, ()) = tokio::join!(execution, observe_pending_authorization);

        assert!(denied.is_err());
        assert!(safe_flash_is_prepared_and_not_executing(
            &state.safe_flash_runtime,
            &session_id
        ));
        assert!(executor.commands().is_empty());
        assert!(state.safe_flash_runtime.cancel(&session_id).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn safe_flash_session_token_validation_waits_for_admission() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingSafeFlashDecision {
                entered: entered.clone(),
                release: release.clone(),
                authorization: OperationAuthorization::allow(),
            })),
            None,
            None,
            None,
        );
        state.session_capabilities.activate();
        let session_id = "safe-missing-token".to_string();
        state
            .safe_flash_runtime
            .replace(prepared_safe_flash_session(&session_id))
            .expect("current session should publish the prepared Safe Flash entry");
        let executor = RecordedSafeFlashExecutor::new(
            Vec::<Result<ProcessOutput, DomainError>>::new(),
        );
        let execution_service = SafeFlashExecutionService::new(Arc::new(executor.clone()));

        let execution = safe_flash_execute_prepared_inner(
            &state,
            session_id.clone(),
            execution_service,
        );
        let observe_pending_authorization = async {
            entered.notified().await;
            assert!(safe_flash_is_prepared_and_not_executing(
                &state.safe_flash_runtime,
                &session_id
            ));
            assert!(executor.commands().is_empty());
            assert!(matches!(
                state.operation_coordinator.try_acquire_idle(),
                Err(OperationCoordinatorError::InProgress)
            ));
            release.notify_one();
        };
        let (result, ()) = tokio::join!(execution, observe_pending_authorization);

        assert!(result.is_err());
        assert!(safe_flash_is_prepared_and_not_executing(
            &state.safe_flash_runtime,
            &session_id
        ));
        assert!(executor.commands().is_empty());
        assert!(state.safe_flash_runtime.cancel(&session_id).is_ok());
    }

    #[tokio::test]
    async fn failed_safe_flash_execution_rolls_back_the_claim_for_retry_or_cancel() {
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::default();
        state.session_capabilities.activate();
        *state
            .session_token
            .write()
            .expect("session token lock should not be poisoned") =
            Some(SecretToken::new("test-token".to_string()));
        state.device_runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::FastbootConnected,
                serial: "CURRENT-DEVICE".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );
        let session_id = "safe-execution-failure".to_string();
        state
            .safe_flash_runtime
            .replace(prepared_safe_flash_session(&session_id))
            .expect("current session should publish the prepared Safe Flash entry");
        let executor = RecordedSafeFlashExecutor::new([Err(DomainError::ExternalTool(
            "injected test failure".to_string(),
        ))]);
        let execution_service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
            .with_fastbootd_wait(1, std::time::Duration::ZERO);

        let result =
            safe_flash_execute_prepared_inner(&state, session_id.clone(), execution_service).await;

        assert!(result.is_err());
        assert_eq!(executor.commands().len(), 1);
        assert!(safe_flash_is_prepared_and_not_executing(
            &state.safe_flash_runtime,
            &session_id
        ));
        assert!(state.safe_flash_runtime.cancel(&session_id).is_ok());
    }

    #[tokio::test]
    async fn successful_safe_flash_execution_completes_the_session_inside_admission() {
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::default();
        state.session_capabilities.activate();
        *state
            .session_token
            .write()
            .expect("session token lock should not be poisoned") =
            Some(SecretToken::new("test-token".to_string()));
        state.device_runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::FastbootConnected,
                serial: "CURRENT-DEVICE".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );
        let session_id = "safe-execution-success".to_string();
        state
            .safe_flash_runtime
            .replace(prepared_safe_flash_session(&session_id))
            .expect("current session should publish the prepared Safe Flash entry");
        let executor = RecordedSafeFlashExecutor::new([
            successful_process_output("CURRENT-DEVICE\tfastboot\n"),
            successful_process_output("(bootloader) is-userspace: yes\n"),
            successful_process_output("(bootloader) partition-type:boot: raw\n"),
            successful_process_output(""),
            successful_process_output(""),
        ]);
        let execution_service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
            .with_fastbootd_wait(1, std::time::Duration::ZERO);

        let result =
            safe_flash_execute_prepared_inner(&state, session_id.clone(), execution_service).await;

        assert!(result.is_ok());
        assert!(!executor.commands().is_empty());
        let runtime_state = state
            .safe_flash_runtime
            .state
            .lock()
            .expect("safe flash runtime lock should not be poisoned");
        assert!(runtime_state.prepared.is_none());
        assert!(runtime_state.executing.is_none());
    }

    #[tokio::test]
    async fn prepared_execution_uses_current_runtime_target_instead_of_preflight_serial() {
        let mut state = AppState::new();
        state.operation_coordinator =
            nwflash_application::OperationCoordinator::new(None, None, None, None, None);
        state.device_runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::FastbootConnected,
                serial: "CURRENT-DEVICE-B".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );
        let prepared = SafeFlashPreparedRequest {
            source: SafeFlashPreparedSource {
                staging_root: None,
                partitions: vec![nwflash_application::SafeFlashPartitionSource {
                    partition_name: "boot".to_string(),
                    image_path: "C:\\staging\\boot.img".to_string(),
                    has_slot: false,
                }],
                wipe_data_image_path: None,
                has_block_based_content: false,
            },
            options: SafeFlashBuildOptions {
                serial: "PREFLIGHT-DEVICE-A".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        };
        let executor = RecordedSafeFlashExecutor::new([
            successful_process_output("CURRENT-DEVICE-B\tfastboot\n"),
            successful_process_output("(bootloader) is-userspace: yes\n"),
            successful_process_output("(bootloader) partition-type:boot: raw\n"),
            successful_process_output(""),
            successful_process_output(""),
        ]);
        let execution_service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
            .with_fastbootd_wait(1, std::time::Duration::ZERO);

        let result = execute_prepared_safe_flash(&state, prepared, execution_service)
            .await
            .expect("command execution must not reject a changed preflight serial");

        assert_eq!(result.flashed_partition_count, 1);
        let commands = executor.commands();
        assert_eq!(commands[0].args, ["devices"]);
        assert!(commands.iter().skip(1).all(|command| {
            command.args.first().map(String::as_str) == Some("-s")
                && command.args.get(1).map(String::as_str) == Some("CURRENT-DEVICE-B")
        }));
    }

    #[tokio::test]
    async fn prepared_execution_uses_the_target_that_is_current_after_authorization() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingSafeFlashAuthorization {
                entered: entered.clone(),
                release: release.clone(),
            })),
            None,
            None,
            None,
        );
        state.device_runtime.apply_snapshot(
            DeviceSnapshot {
                connection_state: DeviceConnectionState::AdbConnected,
                serial: "DEVICE-A".to_string(),
                connection_label: "ADB 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            DeviceRefreshMode::Manual,
        );
        let prepared = SafeFlashPreparedRequest {
            source: SafeFlashPreparedSource {
                staging_root: None,
                partitions: vec![nwflash_application::SafeFlashPartitionSource {
                    partition_name: "boot".to_string(),
                    image_path: "C:\\staging\\boot.img".to_string(),
                    has_slot: false,
                }],
                wipe_data_image_path: None,
                has_block_based_content: false,
            },
            options: SafeFlashBuildOptions {
                serial: "PREFLIGHT-A".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        };
        let executor = RecordedSafeFlashExecutor::new([
            successful_process_output(""),
            successful_process_output("DEVICE-B\tfastboot\n"),
            successful_process_output("(bootloader) is-userspace: yes\n"),
            successful_process_output("(bootloader) partition-type:boot: raw\n"),
            successful_process_output(""),
            successful_process_output(""),
        ]);
        let execution_service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
            .with_fastbootd_wait(1, std::time::Duration::ZERO);

        let execution = execute_prepared_safe_flash(&state, prepared, execution_service);
        let change_target = async {
            entered.notified().await;
            state.device_runtime.apply_snapshot(
                DeviceSnapshot {
                    connection_state: DeviceConnectionState::AdbConnected,
                    serial: "DEVICE-B".to_string(),
                    connection_label: "ADB 已连接".to_string(),
                    model: "--".to_string(),
                    android_version: "--".to_string(),
                    battery_level: "--".to_string(),
                },
                false,
                DeviceRefreshMode::Manual,
            );
            release.notify_one();
        };
        let (result, ()) = tokio::join!(execution, change_target);

        result.expect("the newly current device should be used after authorization");
        let commands = executor.commands();
        assert_eq!(commands[0].args, ["-s", "DEVICE-B", "reboot", "fastboot"]);
        assert!(commands.iter().skip(1).all(|command| {
            command.args.first().map(String::as_str) != Some("-s")
                || command.args.get(1).map(String::as_str) == Some("DEVICE-B")
        }));
    }

    #[test]
    fn prepared_cancel_is_rejected_while_execution_owns_the_staging() {
        let runtime = active_safe_flash_runtime();
        runtime
            .replace(PreparedSafeFlashSession {
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
            })
            .expect("current session should publish the prepared Safe Flash entry");

        runtime
            .begin_execution("safe-running")
            .expect("execution should claim the prepared session");

        assert!(runtime.cancel("safe-running").is_err());
        assert!(runtime.get("safe-running").is_ok());
    }

    #[test]
    fn execution_claim_is_an_atomic_transition_against_prepared_cancellation() {
        let runtime = active_safe_flash_runtime();
        runtime
            .replace(PreparedSafeFlashSession {
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
            })
            .expect("current session should publish the prepared Safe Flash entry");

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
    fn safe_flash_reports_when_the_online_firmware_catalog_has_no_matching_record() {
        let error = sanitize_safe_flash_error("服务端错误: 服务端返回 404: record not found");

        assert_eq!(
            error,
            "在线固件库没有找到匹配的设备版本，请确认设备型号和系统版本。"
        );
    }

    #[test]
    fn safe_flash_reports_authentication_and_credit_failures_separately() {
        assert_eq!(
            sanitize_safe_flash_error("服务端错误: 服务端返回 401: API token 无效或已停用。"),
            "在线固件服务登录状态已失效，请重新登录。"
        );
        assert_eq!(
            sanitize_safe_flash_error("服务端错误: 服务端返回 402: INSUFFICIENT_CREDITS"),
            "在线固件服务余额不足，请联系管理员。"
        );
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
        let runtime = active_safe_flash_runtime();
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
        let runtime = active_safe_flash_runtime();
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
    fn stale_safe_flash_publication_after_session_invalidation_deletes_only_its_candidate_staging()
    {
        let root = std::env::temp_dir().join(format!(
            "nwflash-safe-stale-publish-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be available")
                .as_nanos()
        ));
        let candidate = root.join("candidate-owned");
        let unrelated = root.join("unrelated");
        fs::create_dir_all(&candidate).expect("candidate staging should be created");
        fs::create_dir_all(&unrelated).expect("unrelated directory should be created");
        fs::write(candidate.join("boot.img"), b"staged")
            .expect("candidate image should be written");
        let state = AppState::new();
        state.session_capabilities.activate();
        let stale_lease = state
            .safe_flash_runtime
            .capture_lease()
            .expect("preparation should capture the current session");
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should permit invalidation");
        state.revoke_root_capabilities(&idle_lease);
        state.session_capabilities.activate();
        let prepared = SafeFlashPreparedRequest {
            source: SafeFlashPreparedSource {
                staging_root: Some(candidate.clone()),
                partitions: vec![nwflash_application::SafeFlashPartitionSource {
                    partition_name: "boot".to_string(),
                    image_path: candidate.join("boot.img").to_string_lossy().into_owned(),
                    has_slot: false,
                }],
                wipe_data_image_path: None,
                has_block_based_content: false,
            },
            options: SafeFlashBuildOptions {
                serial: "DEVICE-A".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        };

        let result = build_secure_preflight_with_lease(
            &state.safe_flash_runtime,
            stale_lease,
            "late candidate".to_string(),
            prepared,
        );

        assert!(result.is_err());
        assert!(!candidate.exists());
        assert!(unrelated.is_dir());
        fs::remove_dir_all(root).expect("fixture root should be removed");
    }

    #[test]
    fn session_revocation_rejects_old_safe_flash_id_and_deletes_only_owned_staging() {
        let root = std::env::temp_dir().join(format!(
            "nwflash-safe-session-revoke-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should be available")
                .as_nanos()
        ));
        let owned = root.join("owned");
        let user_image = root.join("user.img");
        fs::create_dir_all(&owned).expect("owned staging should be created");
        fs::write(owned.join("boot.img"), b"staged").expect("staged image should be written");
        fs::write(&user_image, b"user").expect("user image should be written");
        let state = AppState::new();
        state.session_capabilities.activate();
        state
            .safe_flash_runtime
            .replace(PreparedSafeFlashSession {
                id: "safe-old-session".to_string(),
                prepared: SafeFlashPreparedRequest {
                    source: SafeFlashPreparedSource {
                        staging_root: Some(owned.clone()),
                        partitions: vec![nwflash_application::SafeFlashPartitionSource {
                            partition_name: "boot".to_string(),
                            image_path: user_image.to_string_lossy().into_owned(),
                            has_slot: false,
                        }],
                        wipe_data_image_path: None,
                        has_block_based_content: false,
                    },
                    options: SafeFlashBuildOptions {
                        serial: "DEVICE-A".to_string(),
                        is_safe_flash: false,
                        is_keep_root: false,
                        wipe_data: false,
                        wipe_data_image_path: None,
                        slot_mode: SafeFlashSlotMode::CurrentSlot,
                        current_slot: None,
                    },
                },
            })
            .expect("current session should publish the prepared Safe Flash entry");
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should permit session revocation");

        state.revoke_root_capabilities(&idle_lease);
        state.session_capabilities.activate();

        assert!(state.safe_flash_runtime.get("safe-old-session").is_err());
        assert!(!owned.exists());
        assert_eq!(
            fs::read(&user_image).expect("user image must remain"),
            b"user"
        );
        fs::remove_dir_all(root).expect("fixture root should be removed");
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
