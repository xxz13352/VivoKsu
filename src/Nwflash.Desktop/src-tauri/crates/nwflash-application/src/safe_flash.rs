//! Safe flash planning and source preparation utilities for the VIVO flashing workflow.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use zip::read::ZipArchive;

use crate::{
    FirmwareExtractApplicationError, FirmwareExtractEntry, FirmwareExtractService,
    QuickFlashService,
};
use nwflash_domain::{
    compute_targets, is_slot_based_mode, other_slot, should_skip_safe_flash_partition, DomainError,
    PartitionExecutionPlan, PartitionOperationKind, PartitionTask, PartitionTransportKind,
    SafeFlashSlotMode,
};
use nwflash_infrastructure::{
    build_download_target_path, download_to_file_with_cancellation, validate_available_space,
    write_wipe_data_image, EmbeddedAssetError, OtaDiskSpaceProvider, OtaDownloadError,
    OtaDownloadProgressSink, SystemOtaDiskSpaceProvider,
};
use nwflash_windows::{
    device_transport::DeviceTransport,
    platform_tools::PlatformTools,
    process::{
        CancellableProcessExecutor, ProcessCommand, ProcessOutput, SystemCancellableProcessExecutor,
    },
};
use tokio_util::sync::CancellationToken;

static SAFE_FLASH_STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeFlashPreparationPhase {
    ZipExtraction,
    PayloadStaging,
    PayloadExtraction,
}

pub type SafeFlashPreparationProgressSink =
    dyn Fn(SafeFlashPreparationPhase, u64, u64) + Send + Sync;

#[derive(Debug, Clone)]
pub struct SafeFlashPartitionSource {
    pub partition_name: String,
    pub image_path: String,
    pub has_slot: bool,
}

#[derive(Debug, Clone)]
pub struct SafeFlashBuildOptions {
    pub serial: String,
    pub is_safe_flash: bool,
    pub is_keep_root: bool,
    pub wipe_data: bool,
    pub wipe_data_image_path: Option<String>,
    pub slot_mode: SafeFlashSlotMode,
    pub current_slot: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SafeFlashSource {
    LocalPath {
        path: String,
    },
    Online {
        url: String,
        pd: String,
        version: String,
        payload_dumper: Option<PathBuf>,
    },
}

#[derive(Debug, Clone)]
pub struct SafeFlashPreparedSource {
    pub staging_root: Option<PathBuf>,
    pub partitions: Vec<SafeFlashPartitionSource>,
    pub wipe_data_image_path: Option<String>,
    pub has_block_based_content: bool,
}

pub struct SafeFlashExecutionRequest<'a> {
    pub source: &'a SafeFlashPreparedSource,
    pub options: &'a SafeFlashBuildOptions,
    /// The device target resolved immediately before execution. It is used for
    /// an optional ADB-to-fastbootd transition; fastboot commands target the
    /// sole device discovered after the transition.
    pub serial: &'a str,
    pub transition_to_fastbootd: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeFlashExecutionResult {
    pub command_count: usize,
    pub executed_command_count: usize,
    pub flashed_partition_count: usize,
    pub skipped_partition_count: usize,
}

#[derive(Clone)]
pub struct SafeFlashExecutionService {
    executor: Arc<dyn CancellableProcessExecutor>,
    tools: PlatformTools,
    fastbootd_attempts: usize,
    fastbootd_poll_interval: Duration,
}

impl SafeFlashExecutionService {
    const DEFAULT_FASTBOOTD_ATTEMPTS: usize = 120;

    pub fn new(executor: Arc<dyn CancellableProcessExecutor>) -> Self {
        Self {
            executor,
            tools: PlatformTools::bundled(),
            fastbootd_attempts: Self::DEFAULT_FASTBOOTD_ATTEMPTS,
            fastbootd_poll_interval: Duration::from_millis(500),
        }
    }

    pub fn system() -> Self {
        Self::new(Arc::new(SystemCancellableProcessExecutor))
    }

    pub fn with_fastbootd_wait(mut self, attempts: usize, poll_interval: Duration) -> Self {
        self.fastbootd_attempts = attempts.max(1);
        self.fastbootd_poll_interval = poll_interval;
        self
    }

    pub fn execute<F, S, P>(
        &self,
        request: SafeFlashExecutionRequest<'_>,
        mut is_canceled: F,
        mut report_stage: S,
        mut report_progress: P,
    ) -> Result<SafeFlashExecutionResult, DomainError>
    where
        F: FnMut() -> bool,
        S: FnMut(String),
        P: FnMut(f64),
    {
        let transport = DeviceTransport::new(self.tools.clone());
        let mut serial = request.serial.to_owned();
        let mut executed_command_count = 0usize;

        if request.transition_to_fastbootd {
            if is_network_adb_serial(&serial) {
                return Err(DomainError::InvalidOperation(
                    "网络 ADB 设备不支持自动切换到 fastbootd，请使用 USB 连接后重试。".to_string(),
                ));
            }
            report_stage("正在重启到 fastbootd".to_string());
            let command = transport
                .build_adb_reboot_fastboot_command(&serial)
                .map_err(|error| {
                    DomainError::InvalidOperation(format!("重启到 fastbootd 失败：{error}"))
                })?;
            self.run_required(command, &mut is_canceled, "重启到 fastbootd")?;
            executed_command_count += 1;
        }
        report_stage("正在等待 fastbootd".to_string());
        serial = self.wait_for_fastbootd(&transport, &mut is_canceled)?;

        let current_slot = if is_slot_based_mode(request.options.slot_mode) {
            let current_slot = self.read_fastboot_var(
                &transport,
                &serial,
                "current-slot",
                &mut is_canceled,
            )?;
            let normalized_slot = normalize_slot_name(&current_slot);
            if request.options.slot_mode == SafeFlashSlotMode::OtherSlot
                && normalized_slot.is_none()
            {
                return Err(DomainError::InvalidOperation(
                    "未读取到有效 current-slot 值。".to_string(),
                ));
            }
            normalized_slot
        } else {
            None
        };

        let mut flash_commands = Vec::new();
        let mut skipped_partition_count = 0usize;
        for source in &request.source.partitions {
            self.ensure_not_canceled(&mut is_canceled)?;
            if !SafeFlashService::new().is_partition_included(
                &source.partition_name,
                request.options.is_safe_flash,
                request.options.is_keep_root,
            ) {
                continue;
            }

            let has_slot = if is_slot_based_mode(request.options.slot_mode) {
                let variable = format!("has-slot:{}", source.partition_name);
                let has_slot = self.read_fastboot_var(
                    &transport,
                    &serial,
                    &variable,
                    &mut is_canceled,
                )?;
                parse_slot_flag(&has_slot).ok_or_else(|| {
                    DomainError::InvalidOperation(format!("未读取到有效 {variable} 值。"))
                })?
            } else {
                source.has_slot
            };
            for target in compute_targets(
                &source.partition_name,
                request.options.slot_mode,
                current_slot.as_deref(),
                has_slot,
            ) {
                if !self.fastboot_partition_exists(
                    &transport,
                    &serial,
                    &target,
                    &mut is_canceled,
                )? {
                    skipped_partition_count += 1;
                    report_stage(format!("跳过不存在分区：{target}"));
                    continue;
                }
                flash_commands.push(
                    transport
                        .build_fastboot_flash_command(&serial, &target, &source.image_path)
                        .map_err(|error| {
                            DomainError::InvalidOperation(format!("生成刷写命令失败：{error}"))
                        })?,
                );
            }
        }

        if flash_commands.is_empty() {
            return Err(DomainError::InvalidOperation(
                "未发现可刷写分区（可能设备分区与固件不匹配）。".to_string(),
            ));
        }

        let mut commands = flash_commands
            .into_iter()
            .map(|command| (command, true))
            .collect::<Vec<_>>();
        if request.options.slot_mode == SafeFlashSlotMode::OtherSlot {
            if let Some(next_slot) = other_slot(current_slot.as_deref()) {
                commands.push((
                    transport
                        .build_fastboot_set_active_command(&serial, next_slot)
                        .map_err(|error| {
                            DomainError::InvalidOperation(format!("切换槽位失败：{error}"))
                        })?,
                    false,
                ));
            }
        }
        if request.options.wipe_data {
            let image_path = request
                .options
                .wipe_data_image_path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| {
                    DomainError::InvalidInput("清除数据镜像路径不能为空。".to_string())
                })?;
            if self.fastboot_partition_exists(
                &transport,
                &serial,
                SafeFlashService::WIPE_DATA_PARTITION,
                &mut is_canceled,
            )? {
                commands.push((
                    transport
                        .build_fastboot_flash_command(
                            &serial,
                            SafeFlashService::WIPE_DATA_PARTITION,
                            image_path,
                        )
                        .map_err(|error| {
                            DomainError::InvalidOperation(format!("生成清除数据命令失败：{error}"))
                        })?,
                    true,
                ));
            } else {
                skipped_partition_count += 1;
            }
        }
        commands.push((
            transport
                .build_fastboot_reboot_command(&serial)
                .map_err(|error| DomainError::InvalidOperation(format!("重启设备失败：{error}")))?,
            false,
        ));

        let command_total = commands.len();
        let command_count = command_total + usize::from(request.transition_to_fastbootd);
        let mut flashed_partition_count = 0usize;
        for (index, (command, is_flash)) in commands.into_iter().enumerate() {
            self.ensure_not_canceled(&mut is_canceled)?;
            report_stage(format!(
                "执行 {}/{}: {}",
                index + 1,
                command_total,
                command.program
            ));
            report_progress((index + 1) as f64 / command_total as f64);
            self.run_required(command, &mut is_canceled, "fastboot 命令")?;
            executed_command_count += 1;
            if is_flash {
                flashed_partition_count += 1;
            }
        }

        Ok(SafeFlashExecutionResult {
            command_count,
            executed_command_count,
            flashed_partition_count,
            skipped_partition_count,
        })
    }

    fn ensure_not_canceled<F>(&self, is_canceled: &mut F) -> Result<(), DomainError>
    where
        F: FnMut() -> bool,
    {
        if is_canceled() {
            return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
        }
        Ok(())
    }

    fn run_required<F>(
        &self,
        command: ProcessCommand,
        is_canceled: &mut F,
        label: &str,
    ) -> Result<ProcessOutput, DomainError>
    where
        F: FnMut() -> bool,
    {
        self.ensure_not_canceled(is_canceled)?;
        let output = self
            .executor
            .run(command, is_canceled)
            .map_err(|error| match error {
                DomainError::UserCancelled(_) => {
                    DomainError::UserCancelled("运行被用户取消".to_string())
                }
                _ => DomainError::ExternalTool(format!("{label}执行失败。")),
            })?;
        if output.exit_code == 0 {
            Ok(output)
        } else {
            Err(DomainError::ExternalTool(format!(
                "{label}执行失败，退出码 {}。",
                output.exit_code
            )))
        }
    }

    fn wait_for_fastbootd<F>(
        &self,
        transport: &DeviceTransport,
        is_canceled: &mut F,
    ) -> Result<String, DomainError>
    where
        F: FnMut() -> bool,
    {
        for attempt in 0..self.fastbootd_attempts {
            self.ensure_not_canceled(is_canceled)?;
            let output = self.run_required(
                self.tools.fastboot_devices_command().map_err(|error| {
                    DomainError::InvalidOperation(format!("检测 fastbootd 失败：{error}"))
                })?,
                is_canceled,
                "检测 fastbootd",
            )?;
            if let Some(serial) = sole_fastboot_device_serial(&output.stdout)? {
                let userspace =
                    self.read_fastboot_var(transport, &serial, "is-userspace", is_canceled)?;
                if is_affirmative_flag(&userspace) {
                    return Ok(serial);
                }
            }
            if attempt + 1 < self.fastbootd_attempts {
                thread::sleep(self.fastbootd_poll_interval);
            }
        }
        Err(DomainError::DeviceUnavailable(
            "无法确认唯一 fastboot 设备已进入 fastbootd，已取消线刷。".to_string(),
        ))
    }

    fn read_fastboot_var<F>(
        &self,
        transport: &DeviceTransport,
        serial: &str,
        variable: &str,
        is_canceled: &mut F,
    ) -> Result<String, DomainError>
    where
        F: FnMut() -> bool,
    {
        let output = self.run_required(
            transport
                .build_fastboot_getvar_command(serial, variable)
                .map_err(|error| {
                    DomainError::InvalidOperation(format!("读取 fastboot 变量失败：{error}"))
                })?,
            is_canceled,
            &format!("读取 {variable}"),
        )?;
        let combined_output = format!("{}\n{}", output.stdout, output.stderr);
        parse_fastboot_var_output(&combined_output, variable)
            .ok_or_else(|| DomainError::InvalidOperation(format!("未读取到 {variable} 值。")))
    }

    fn fastboot_partition_exists<F>(
        &self,
        transport: &DeviceTransport,
        serial: &str,
        partition: &str,
        is_canceled: &mut F,
    ) -> Result<bool, DomainError>
    where
        F: FnMut() -> bool,
    {
        self.ensure_not_canceled(is_canceled)?;
        let command = transport
            .build_fastboot_getvar_command(serial, &format!("partition-type:{partition}"))
            .map_err(|error| DomainError::InvalidOperation(format!("读取分区类型失败：{error}")))?;
        let output = self
            .executor
            .run(command, is_canceled)
            .map_err(|error| match error {
                DomainError::UserCancelled(_) => {
                    DomainError::UserCancelled("运行被用户取消".to_string())
                }
                _ => DomainError::ExternalTool(format!("读取分区 {partition} 失败。")),
            })?;
        if output.exit_code == 0 {
            return Ok(true);
        }
        if is_missing_partition_error(&output.stdout) || is_missing_partition_error(&output.stderr)
        {
            return Ok(false);
        }
        Err(DomainError::ExternalTool(format!(
            "读取分区 {partition} 失败，退出码 {}。",
            output.exit_code
        )))
    }
}

fn sole_fastboot_device_serial(output: &str) -> Result<Option<String>, DomainError> {
    let mut serials = output.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let serial = fields.next()?;
        let state = fields.next()?;
        state
            .eq_ignore_ascii_case("fastboot")
            .then(|| serial.to_string())
    });
    let Some(serial) = serials.next() else {
        return Ok(None);
    };
    if serials.next().is_some() {
        return Err(DomainError::DeviceUnavailable(
            "检测到多个 fastboot 设备，请仅连接一台设备后重试。".to_string(),
        ));
    }
    Ok(Some(serial))
}

fn is_network_adb_serial(serial: &str) -> bool {
    serial
        .trim()
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok())
}

fn parse_fastboot_var_output(output: &str, variable: &str) -> Option<String> {
    let prefix = format!("{variable}:");
    output.lines().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("(bootloader)").unwrap_or(line).trim();
        line.get(..prefix.len())
            .filter(|candidate| candidate.eq_ignore_ascii_case(&prefix))
            .map(|_| line[prefix.len()..].trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn normalize_slot_name(value: &str) -> Option<String> {
    match value.trim().trim_start_matches('_').to_lowercase().as_str() {
        "a" => Some("a".to_string()),
        "b" => Some("b".to_string()),
        _ => None,
    }
}

fn is_affirmative_flag(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "yes" | "1" | "true" | "on"
    )
}

fn parse_slot_flag(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "yes" | "1" | "true" | "on" => Some(true),
        "no" | "0" | "false" | "off" => Some(false),
        _ => None,
    }
}

fn is_missing_partition_error(text: &str) -> bool {
    let normalized = text.to_lowercase();
    normalized.contains("unknown partition")
        || normalized.contains("partition not found")
        || normalized.contains("does not exist")
        || normalized.contains("unknown variable")
}

#[derive(Debug, Clone)]
pub struct SafeFlashService;

impl Default for SafeFlashService {
    fn default() -> Self {
        Self
    }
}

impl SafeFlashService {
    pub const WIPE_DATA_PARTITION: &'static str = "misc";
    const WIPE_DATA_FILENAME: &'static str = "wipe-data.img";

    pub fn new() -> Self {
        Self
    }

    pub fn build_plan(
        &self,
        partitions: &[SafeFlashPartitionSource],
        options: SafeFlashBuildOptions,
    ) -> Result<PartitionExecutionPlan, DomainError> {
        if options.serial.is_empty() {
            return Err(DomainError::InvalidInput(
                "设备序列号不能为空。".to_string(),
            ));
        }

        let mut tasks: Vec<PartitionTask> = Vec::new();

        for source in partitions {
            let partition_name = source.partition_name.trim();
            if partition_name.is_empty() {
                return Err(DomainError::InvalidInput("分区名不能为空。".to_string()));
            }

            if source.image_path.trim().is_empty() {
                return Err(DomainError::InvalidInput(format!(
                    "分区 {partition_name} 的镜像路径不能为空。"
                )));
            }

            if !self.is_partition_included(
                partition_name,
                options.is_safe_flash,
                options.is_keep_root,
            ) {
                continue;
            }

            let targets = if is_slot_based_mode(options.slot_mode) {
                compute_targets(
                    partition_name,
                    options.slot_mode,
                    options.current_slot.as_deref(),
                    source.has_slot,
                )
            } else {
                vec![partition_name.to_string()]
            };

            for target in targets {
                tasks.push(PartitionTask {
                    partition_name: target.clone(),
                    device_path: target,
                    image_path: Some(source.image_path.clone()),
                    output_path: None,
                    size_bytes: None,
                });
            }
        }

        if options.wipe_data {
            let wipe_data_path = options
                .wipe_data_image_path
                .as_ref()
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| {
                    DomainError::InvalidInput("清除数据镜像路径不能为空。".to_string())
                })?;

            tasks.push(PartitionTask {
                partition_name: Self::WIPE_DATA_PARTITION.to_string(),
                device_path: Self::WIPE_DATA_PARTITION.to_string(),
                image_path: Some(wipe_data_path.clone()),
                output_path: None,
                size_bytes: None,
            });
        }

        if tasks.is_empty() {
            return Err(DomainError::InvalidOperation(
                "请至少选择一个可刷写分区。".to_string(),
            ));
        }

        Ok(PartitionExecutionPlan {
            serial: options.serial,
            transport: PartitionTransportKind::Fastboot,
            operation: PartitionOperationKind::Write,
            tasks,
        })
    }

    pub fn build_commands(
        &self,
        partitions: &[SafeFlashPartitionSource],
        options: SafeFlashBuildOptions,
    ) -> Result<Vec<crate::CommandSpec>, DomainError> {
        let plan = self.build_plan(partitions, options)?;
        let quick_flash_service = QuickFlashService::with_default_tools();
        quick_flash_service.build_commands(&plan)
    }

    pub async fn resolve_source(
        &self,
        source: SafeFlashSource,
        options: &SafeFlashBuildOptions,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.resolve_source_with_cancellation(source, options, &CancellationToken::new(), None)
            .await
    }

    pub async fn resolve_source_with_cancellation(
        &self,
        source: SafeFlashSource,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
        download_progress: Option<Arc<OtaDownloadProgressSink>>,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.resolve_source_with_cancellation_and_progress(
            source,
            options,
            cancellation,
            download_progress,
            None,
        )
        .await
    }

    pub async fn resolve_source_with_cancellation_and_progress(
        &self,
        source: SafeFlashSource,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
        download_progress: Option<Arc<OtaDownloadProgressSink>>,
        preparation_progress: Option<Arc<SafeFlashPreparationProgressSink>>,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        match source {
            SafeFlashSource::LocalPath { path } => {
                self.resolve_local_source(
                    Path::new(&path),
                    options,
                    cancellation,
                    preparation_progress.as_ref(),
                )
                .await
            }
            SafeFlashSource::Online {
                url,
                pd,
                version,
                payload_dumper,
            } => {
                self.resolve_online_source(
                    &url,
                    &pd,
                    &version,
                    options,
                    cancellation,
                    download_progress,
                    payload_dumper.as_deref(),
                    preparation_progress.as_ref(),
                )
                .await
            }
        }
    }

    pub fn resolve_payload_source(
        &self,
        executable_path: &Path,
        payload_source: &Path,
        options: &SafeFlashBuildOptions,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.resolve_payload_source_with_cancellation(
            executable_path,
            payload_source,
            options,
            &CancellationToken::new(),
        )
    }

    pub fn resolve_payload_source_with_cancellation(
        &self,
        executable_path: &Path,
        payload_source: &Path,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.resolve_payload_source_with_cancellation_and_progress(
            executable_path,
            payload_source,
            options,
            cancellation,
            None,
        )
    }

    pub fn resolve_payload_source_with_cancellation_and_progress(
        &self,
        executable_path: &Path,
        payload_source: &Path,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
        preparation_progress: Option<&Arc<SafeFlashPreparationProgressSink>>,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.ensure_preparation_not_canceled(cancellation)?;
        let staging_root = self.create_staging_root();
        std::fs::create_dir_all(&staging_root)
            .map_err(|error| DomainError::InvalidOperation(format!("创建临时目录失败：{error}")))?;
        let result = (|| {
            let payload_source = self.stage_payload_source(
                payload_source,
                &staging_root,
                cancellation,
                preparation_progress,
            )?;
            let payload_source = payload_source.to_str().ok_or_else(|| {
                DomainError::InvalidInput("本地 payload 路径包含不支持的字符。".to_string())
            })?;
            let metadata_directory = staging_root.join("metadata");
            let inspection = FirmwareExtractService::inspect_payload(
                executable_path,
                payload_source,
                &metadata_directory,
                || cancellation.is_cancelled(),
            )
            .map_err(map_firmware_extract_error)?;
            self.ensure_preparation_not_canceled(cancellation)?;
            let _ = std::fs::remove_dir_all(&metadata_directory);
            let selected = inspection
                .entries
                .into_iter()
                .filter(|entry| {
                    self.is_partition_included(
                        &entry.name,
                        options.is_safe_flash,
                        options.is_keep_root,
                    )
                })
                .collect::<Vec<_>>();
            if selected.is_empty() {
                return Err(DomainError::InvalidOperation(
                    "payload 中没有可刷写分区。".to_string(),
                ));
            }
            let payload_output_bytes = checked_payload_output_size(&selected)?;
            let staged_payload_bytes = if Path::new(payload_source).starts_with(&staging_root) {
                std::fs::metadata(payload_source)
                    .map_err(|error| {
                        DomainError::InvalidOperation(format!("读取暂存 payload 失败：{error}"))
                    })?
                    .len()
            } else {
                0
            };
            let required_bytes = staged_payload_bytes
                .checked_add(payload_output_bytes)
                .ok_or_else(|| {
                    DomainError::InvalidOperation("payload 解包所需空间超出支持范围。".to_string())
                })?;
            let image_directory = staging_root.join("images");
            self.ensure_extraction_capacity(&staging_root, required_bytes)?;
            let images = FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
                executable_path,
                payload_source,
                &selected,
                &image_directory,
                || cancellation.is_cancelled(),
                |_, written_bytes| {
                    report_preparation_progress(
                        preparation_progress,
                        SafeFlashPreparationPhase::PayloadExtraction,
                        written_bytes,
                        payload_output_bytes,
                    );
                },
            )
            .map_err(map_firmware_extract_error)?;
            self.ensure_preparation_not_canceled(cancellation)?;
            if images.len() != selected.len() {
                return Err(DomainError::InvalidOperation(
                    "payload 提取结果不完整。".to_string(),
                ));
            }
            let partitions = selected
                .into_iter()
                .zip(images)
                .map(|(entry, image)| SafeFlashPartitionSource {
                    partition_name: entry.name,
                    image_path: image.path,
                    has_slot: true,
                })
                .collect();
            let wipe_data_image_path =
                self.resolve_wipe_data_image_path(Some(&staging_root), options)?;
            Ok(SafeFlashPreparedSource {
                staging_root: Some(staging_root.clone()),
                partitions,
                wipe_data_image_path,
                has_block_based_content: false,
            })
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging_root);
        }
        result
    }

    fn stage_payload_source(
        &self,
        source: &Path,
        staging_root: &Path,
        cancellation: &CancellationToken,
        preparation_progress: Option<&Arc<SafeFlashPreparationProgressSink>>,
    ) -> Result<PathBuf, DomainError> {
        self.ensure_preparation_not_canceled(cancellation)?;
        let is_zip = source
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
        if !is_zip {
            return Ok(source.to_path_buf());
        }

        let file = File::open(source).map_err(|error| {
            DomainError::InvalidOperation(format!("打开 payload 压缩包失败：{error}"))
        })?;
        let mut archive = ZipArchive::new(file).map_err(|error| {
            DomainError::InvalidFormat(format!("读取 payload 压缩包失败：{error}"))
        })?;
        let mut payload_index = None;
        for index in 0..archive.len() {
            self.ensure_preparation_not_canceled(cancellation)?;
            let entry = archive.by_index(index).map_err(|error| {
                DomainError::InvalidFormat(format!("读取 payload 压缩包入口失败：{error}"))
            })?;
            let name = entry.name().map_err(|error| {
                DomainError::InvalidFormat(format!("读取 payload 压缩包入口名称失败：{error}"))
            })?;
            let is_payload = Path::new(name.as_ref())
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("payload.bin"));
            if is_payload && payload_index.replace(index).is_some() {
                return Err(DomainError::InvalidFormat(
                    "payload 压缩包包含多个 payload.bin。".to_string(),
                ));
            }
        }
        let payload_index = payload_index.ok_or_else(|| {
            DomainError::InvalidFormat("payload 压缩包不包含 payload.bin。".to_string())
        })?;
        let mut entry = archive.by_index(payload_index).map_err(|error| {
            DomainError::InvalidFormat(format!("读取 payload 压缩包入口失败：{error}"))
        })?;
        let total_bytes = entry.size();
        self.ensure_extraction_capacity(staging_root, total_bytes)?;
        let staged_payload = staging_root.join("payload.bin");
        let partial_payload = staging_root.join("payload.bin.partial");
        let copy_result = (|| {
            let mut output = File::create(&partial_payload).map_err(|error| {
                DomainError::InvalidOperation(format!("创建临时 payload 失败：{error}"))
            })?;
            let mut buffer = [0u8; 64 * 1024];
            let mut copied_bytes = 0u64;
            loop {
                self.ensure_preparation_not_canceled(cancellation)?;
                let count = entry.read(&mut buffer).map_err(|error| {
                    DomainError::InvalidOperation(format!("解压 payload 失败：{error}"))
                })?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count]).map_err(|error| {
                    DomainError::InvalidOperation(format!("写入临时 payload 失败：{error}"))
                })?;
                copied_bytes = copied_bytes.saturating_add(count as u64);
                report_preparation_progress(
                    preparation_progress,
                    SafeFlashPreparationPhase::PayloadStaging,
                    copied_bytes,
                    total_bytes,
                );
            }
            self.ensure_preparation_not_canceled(cancellation)?;
            output.flush().map_err(|error| {
                DomainError::InvalidOperation(format!("写入临时 payload 失败：{error}"))
            })?;
            std::fs::rename(&partial_payload, &staged_payload).map_err(|error| {
                DomainError::InvalidOperation(format!("完成 payload 暂存失败：{error}"))
            })?;
            Ok(staged_payload)
        })();
        if copy_result.is_err() {
            let _ = std::fs::remove_file(&partial_payload);
        }
        copy_result
    }

    async fn resolve_local_source(
        &self,
        path: &Path,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
        preparation_progress: Option<&Arc<SafeFlashPreparationProgressSink>>,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.ensure_preparation_not_canceled(cancellation)?;
        if path.as_os_str().is_empty() {
            return Err(DomainError::InvalidInput(
                "本地源路径不能为空。".to_string(),
            ));
        }

        let meta = std::fs::metadata(path).map_err(|error| {
            DomainError::InvalidOperation(format!(
                "读取本地源失败：{}（{}）",
                path.to_string_lossy(),
                error
            ))
        })?;

        let mut has_block_based_content = false;

        if meta.is_dir() {
            let staging_root = options.wipe_data.then(|| self.create_staging_root());
            if let Some(root) = staging_root.as_deref() {
                std::fs::create_dir_all(root).map_err(|error| {
                    DomainError::InvalidOperation(format!("创建临时目录失败：{error}"))
                })?;
            }
            let result = (|| {
                let partitions = self
                    .list_directory_images(path, options, cancellation)
                    .map_err(map_preparation_io_error)?;
                let wipe_data_image_path =
                    self.resolve_wipe_data_image_path(staging_root.as_deref(), options)?;

                Ok(SafeFlashPreparedSource {
                    staging_root: staging_root.clone(),
                    partitions,
                    wipe_data_image_path,
                    has_block_based_content: false,
                })
            })();
            if result.is_err() {
                if let Some(root) = staging_root.as_deref() {
                    let _ = std::fs::remove_dir_all(root);
                }
            }
            return result;
        }

        if !path.is_file() {
            return Err(DomainError::InvalidOperation(format!(
                "不支持的来源类型：{}",
                path.to_string_lossy()
            )));
        }

        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();
        let staging_root =
            (options.wipe_data || extension == "zip").then(|| self.create_staging_root());
        if let Some(root) = staging_root.as_deref() {
            std::fs::create_dir_all(root).map_err(|error| {
                DomainError::InvalidOperation(format!("创建临时目录失败：{error}"))
            })?;
        }
        let result = async {
            self.ensure_preparation_not_canceled(cancellation)?;
            let partitions = if extension == "zip" {
                has_block_based_content = self
                    .has_block_based_content_with_cancellation(path, cancellation)
                    .map_err(map_preparation_io_error)?;
                self.list_zip_images(
                    path,
                    options,
                    staging_root.as_deref(),
                    cancellation,
                    preparation_progress,
                )
                .await?
            } else if extension == "img" || extension == "bin" {
                self.list_single_image(path)?
            } else {
                return Err(DomainError::InvalidFormat(
                    "仅支持 .zip/.img/.bin 来源。".to_string(),
                ));
            };

            self.ensure_preparation_not_canceled(cancellation)?;
            let wipe_data_image_path =
                self.resolve_wipe_data_image_path(staging_root.as_deref(), options)?;

            Ok(SafeFlashPreparedSource {
                staging_root: staging_root.clone(),
                partitions,
                wipe_data_image_path,
                has_block_based_content,
            })
        }
        .await;
        if result.is_err() {
            if let Some(root) = staging_root.as_deref() {
                let _ = std::fs::remove_dir_all(root);
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_online_source(
        &self,
        url: &str,
        pd: &str,
        version: &str,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
        download_progress: Option<Arc<OtaDownloadProgressSink>>,
        payload_dumper: Option<&Path>,
        preparation_progress: Option<&Arc<SafeFlashPreparationProgressSink>>,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.ensure_preparation_not_canceled(cancellation)?;
        if url.trim().is_empty() {
            return Err(DomainError::InvalidInput(
                "在线 OTA 地址不能为空。".to_string(),
            ));
        }

        let staging_root = self.create_staging_root();
        std::fs::create_dir_all(&staging_root)
            .map_err(|error| DomainError::InvalidOperation(format!("创建临时目录失败：{error}")))?;

        let result = async {
            let download_target =
                build_download_target_path(&staging_root, "safe-flash", pd, version);
            download_to_file_with_cancellation(
                url,
                &download_target,
                cancellation,
                download_progress,
            )
            .await
            .map_err(map_ota_download_error)?;
            self.ensure_preparation_not_canceled(cancellation)?;

            let mut archive = ZipArchive::new(File::open(&download_target).map_err(|error| {
                DomainError::InvalidOperation(format!("打开 OTA 压缩包失败：{error}"))
            })?)
            .map_err(|error| DomainError::InvalidFormat(format!("读取压缩包失败：{error}")))?;
            let has_payload = has_payload_bin(&mut archive, cancellation)?;
            drop(archive);
            if has_payload {
                let executable = payload_dumper.ok_or_else(|| {
                    DomainError::ExternalTool("payload 提取工具未就绪。".to_string())
                })?;
                let prepared = self.resolve_payload_source_with_cancellation_and_progress(
                    executable,
                    &download_target,
                    options,
                    cancellation,
                    preparation_progress,
                )?;
                let _ = std::fs::remove_dir_all(&staging_root);
                return Ok(prepared);
            }

            let partitions = self
                .list_zip_images(
                    &download_target,
                    options,
                    Some(&staging_root),
                    cancellation,
                    preparation_progress,
                )
                .await?;
            let has_block_based_content = self
                .has_block_based_content_with_cancellation(&download_target, cancellation)
                .map_err(map_preparation_io_error)?;

            let wipe_data_image_path =
                self.resolve_wipe_data_image_path(Some(&staging_root), options)?;

            Ok(SafeFlashPreparedSource {
                staging_root: Some(staging_root.clone()),
                partitions,
                wipe_data_image_path,
                has_block_based_content,
            })
        }
        .await;
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staging_root);
        }
        result
    }

    fn ensure_preparation_not_canceled(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), DomainError> {
        if cancellation.is_cancelled() {
            return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
        }
        Ok(())
    }

    fn list_single_image(&self, path: &Path) -> Result<Vec<SafeFlashPartitionSource>, DomainError> {
        let name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                DomainError::InvalidOperation("本地镜像文件名非法，无法解析分区名。".to_string())
            })?;

        if name.eq_ignore_ascii_case("payload") {
            return Err(DomainError::InvalidFormat(
                "不支持 payload.bin 单文件源。".to_string(),
            ));
        }

        Ok(vec![SafeFlashPartitionSource {
            partition_name: name.to_string(),
            image_path: path.to_string_lossy().into_owned(),
            has_slot: true,
        }])
    }

    fn resolve_wipe_data_image_path(
        &self,
        staging_root: Option<&Path>,
        options: &SafeFlashBuildOptions,
    ) -> Result<Option<String>, DomainError> {
        if !options.wipe_data {
            return Ok(options.wipe_data_image_path.clone());
        }

        if let Some(path) = options.wipe_data_image_path.as_ref() {
            if path.trim().is_empty() {
                return Err(DomainError::InvalidInput(
                    "清除数据镜像路径不能为空。".to_string(),
                ));
            }
            return Ok(Some(path.clone()));
        }

        let root = staging_root.ok_or_else(|| {
            DomainError::InvalidOperation("未提供临时目录，无法生成清除镜像。".to_string())
        })?;

        let destination = root.join(Self::WIPE_DATA_FILENAME);
        write_wipe_data_image(&destination).map_err(map_embedded_asset_error)?;
        Ok(Some(destination.to_string_lossy().into_owned()))
    }

    fn is_partition_included(
        &self,
        partition_name: &str,
        is_safe_flash: bool,
        is_keep_root: bool,
    ) -> bool {
        (!is_safe_flash || !should_skip_safe_flash_partition(partition_name))
            && (!is_keep_root || !self.is_boot_partition(partition_name))
    }

    fn is_boot_partition(&self, name: &str) -> bool {
        matches!(
            name.to_lowercase().as_str(),
            "boot" | "init_boot" | "vendor_boot"
        )
    }

    fn list_directory_images(
        &self,
        source: &Path,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
    ) -> Result<Vec<SafeFlashPartitionSource>, io::Error> {
        let mut partitions = Vec::new();
        let mut seen = HashSet::new();

        for entry in std::fs::read_dir(source)? {
            if cancellation.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "线刷预检已取消"));
            }
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let ext = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext != "img" && ext != "bin" {
                continue;
            }

            let partition_name = match path.file_stem().and_then(|value| value.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            if partition_name.eq_ignore_ascii_case("payload") {
                continue;
            }

            if !self.is_partition_included(
                &partition_name,
                options.is_safe_flash,
                options.is_keep_root,
            ) {
                continue;
            }

            if seen.insert(partition_name.clone()) {
                partitions.push(SafeFlashPartitionSource {
                    partition_name,
                    image_path: path.to_string_lossy().to_string(),
                    has_slot: true,
                });
            }
        }

        Ok(partitions)
    }

    async fn list_zip_images(
        &self,
        source: &Path,
        options: &SafeFlashBuildOptions,
        staging_root: Option<&Path>,
        cancellation: &CancellationToken,
        preparation_progress: Option<&Arc<SafeFlashPreparationProgressSink>>,
    ) -> Result<Vec<SafeFlashPartitionSource>, DomainError> {
        self.ensure_preparation_not_canceled(cancellation)?;
        let mut archive = ZipArchive::new(File::open(source).map_err(|error| {
            DomainError::InvalidOperation(format!("打开 OTA 压缩包失败：{error}"))
        })?)
        .map_err(|error| DomainError::InvalidFormat(format!("读取压缩包失败：{error}")))?;

        if has_payload_bin(&mut archive, cancellation)? {
            return Err(DomainError::InvalidFormat(
                "当前 Rust 版未内置 payload.bin 解包工具，请提供解包后的镜像目录或普通 OTA zip。"
                    .to_string(),
            ));
        }

        let mut seen = HashSet::new();
        let output_dir = staging_root.map(Path::to_path_buf).unwrap_or_else(|| {
            source
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        });
        std::fs::create_dir_all(&output_dir)
            .map_err(|error| DomainError::InvalidOperation(format!("创建解包目录失败：{error}")))?;

        let required_bytes = zip_extraction_output_size(&mut archive, options, cancellation)?;
        self.ensure_extraction_capacity(&output_dir, required_bytes)?;

        let mut partitions = Vec::new();
        let mut copied_bytes = 0u64;
        for index in 0..archive.len() {
            self.ensure_preparation_not_canceled(cancellation)?;
            let mut entry = archive.by_index(index).map_err(|error| {
                DomainError::InvalidFormat(format!("读取 OTA 入口失败：{error}"))
            })?;
            let name = entry
                .name()
                .map_err(|error| {
                    DomainError::InvalidFormat(format!("读取 OTA 入口名称失败：{error}"))
                })?
                .to_lowercase();
            if name.ends_with('/') {
                continue;
            }

            let file_name = Path::new(&name)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_string();

            let ext = Path::new(&file_name)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_lowercase();

            if ext != "img" && ext != "bin" {
                continue;
            }

            let partition_name = Path::new(&file_name)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_string();

            if partition_name.eq_ignore_ascii_case("payload") || partition_name.is_empty() {
                continue;
            }

            if !self.is_partition_included(
                &partition_name,
                options.is_safe_flash,
                options.is_keep_root,
            ) {
                continue;
            }

            if !seen.insert(partition_name.clone()) {
                continue;
            }

            let output_path = output_dir.join(format!("{partition_name}.img"));
            let mut output = File::create(&output_path)
                .map_err(|error| DomainError::InvalidOperation(format!("解包失败：{error}")))?;
            let copy_result = (|| {
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    self.ensure_preparation_not_canceled(cancellation)?;
                    let count = entry.read(&mut buffer).map_err(|error| {
                        DomainError::InvalidOperation(format!(
                            "解包分区 {partition_name} 失败：{error}"
                        ))
                    })?;
                    if count == 0 {
                        break;
                    }
                    output.write_all(&buffer[..count]).map_err(|error| {
                        DomainError::InvalidOperation(format!(
                            "解包分区 {partition_name} 失败：{error}"
                        ))
                    })?;
                    copied_bytes = copied_bytes.saturating_add(count as u64);
                    report_preparation_progress(
                        preparation_progress,
                        SafeFlashPreparationPhase::ZipExtraction,
                        copied_bytes,
                        required_bytes,
                    );
                }
                self.ensure_preparation_not_canceled(cancellation)
            })();
            if let Err(error) = copy_result {
                let _ = std::fs::remove_file(&output_path);
                return Err(error);
            }

            partitions.push(SafeFlashPartitionSource {
                partition_name,
                image_path: output_path.to_string_lossy().into_owned(),
                has_slot: true,
            });
        }

        Ok(partitions)
    }

    fn ensure_extraction_capacity(
        &self,
        output_dir: &Path,
        required_bytes: u64,
    ) -> Result<(), DomainError> {
        let available_bytes = SystemOtaDiskSpaceProvider
            .available_bytes(output_dir)
            .map_err(|error| {
                DomainError::InvalidOperation(format!("读取解包磁盘空间失败：{error}"))
            })?;
        validate_extraction_capacity(required_bytes, available_bytes)
    }

    fn has_block_based_content_with_cancellation(
        &self,
        source: &Path,
        cancellation: &CancellationToken,
    ) -> io::Result<bool> {
        if !source.exists() || source.is_dir() {
            return Ok(false);
        }

        if source
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_lowercase()
            != "zip"
        {
            return Ok(false);
        }

        let mut archive = ZipArchive::new(File::open(source)?)?;
        for index in 0..archive.len() {
            if cancellation.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "线刷预检已取消"));
            }
            let entry = archive.by_index(index)?;
            let name = entry
                .name()
                .map_err(|error| io::Error::other(format!("读取 OTA 入口名称失败：{error}")))?
                .to_lowercase();
            if name.ends_with(".new.dat")
                || name.ends_with(".patch.dat")
                || name.ends_with(".transfer.list")
            {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn create_staging_root(&self) -> PathBuf {
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let process = std::process::id();
        let sequence = SAFE_FLASH_STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join("nwflash-safe-flash")
            .join(format!("{process}_{unix}_{sequence}"))
    }
}

fn has_payload_bin(
    archive: &mut ZipArchive<File>,
    cancellation: &CancellationToken,
) -> Result<bool, DomainError> {
    for index in 0..archive.len() {
        if cancellation.is_cancelled() {
            return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
        }
        let entry = archive
            .by_index(index)
            .map_err(|error| DomainError::InvalidFormat(format!("读取 OTA 入口失败：{error}")))?;
        let name = entry
            .name()
            .map_err(|error| DomainError::InvalidFormat(format!("读取 OTA 入口名称失败：{error}")))?
            .to_string();
        if Path::new(&name)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("payload.bin"))
        {
            return Ok(true);
        }
    }

    Ok(false)
}

fn map_ota_download_error(error: OtaDownloadError) -> DomainError {
    match error {
        OtaDownloadError::Cancelled => DomainError::UserCancelled("OTA 下载已取消。".to_string()),
        OtaDownloadError::UnknownContentLength => {
            DomainError::InvalidOperation("OTA 下载失败：无法确定 OTA 包大小。".to_string())
        }
        OtaDownloadError::InvalidInput(message)
        | OtaDownloadError::Download(message)
        | OtaDownloadError::Io(message) => {
            DomainError::InvalidOperation(format!("OTA 下载失败：{message}"))
        }
    }
}

fn report_preparation_progress(
    sink: Option<&Arc<SafeFlashPreparationProgressSink>>,
    phase: SafeFlashPreparationPhase,
    completed_bytes: u64,
    total_bytes: u64,
) {
    if total_bytes > 0 {
        if let Some(sink) = sink {
            sink(phase, completed_bytes.min(total_bytes), total_bytes);
        }
    }
}

fn validate_extraction_capacity(
    required_bytes: u64,
    available_bytes: u64,
) -> Result<(), DomainError> {
    validate_available_space(required_bytes, available_bytes)
        .map_err(|error| DomainError::InvalidOperation(format!("解包磁盘空间不足：{error}")))
}

fn checked_payload_output_size(entries: &[FirmwareExtractEntry]) -> Result<u64, DomainError> {
    entries.iter().try_fold(0u64, |total, entry| {
        let size = u64::try_from(entry.size_bytes).map_err(|_| {
            DomainError::InvalidFormat(format!("payload 分区 {} 的大小非法。", entry.name))
        })?;
        total.checked_add(size).ok_or_else(|| {
            DomainError::InvalidFormat("payload 分区总大小超出支持范围。".to_string())
        })
    })
}

fn zip_extraction_output_size(
    archive: &mut ZipArchive<File>,
    options: &SafeFlashBuildOptions,
    cancellation: &CancellationToken,
) -> Result<u64, DomainError> {
    let mut names = HashSet::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        if cancellation.is_cancelled() {
            return Err(DomainError::UserCancelled("线刷预检已取消。".to_string()));
        }
        let entry = archive
            .by_index(index)
            .map_err(|error| DomainError::InvalidFormat(format!("读取 OTA 入口失败：{error}")))?;
        let name = entry.name().map_err(|error| {
            DomainError::InvalidFormat(format!("读取 OTA 入口名称失败：{error}"))
        })?;
        let file_name = Path::new(name.as_ref())
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let partition_name = Path::new(file_name)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let extension = Path::new(file_name)
            .extension()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if (extension.eq_ignore_ascii_case("img") || extension.eq_ignore_ascii_case("bin"))
            && !partition_name.eq_ignore_ascii_case("payload")
            && !partition_name.is_empty()
            && (!options.is_safe_flash || !should_skip_safe_flash_partition(partition_name))
            && (!options.is_keep_root
                || !matches!(
                    partition_name.to_ascii_lowercase().as_str(),
                    "boot" | "init_boot" | "vendor_boot"
                ))
            && names.insert(partition_name.to_ascii_lowercase())
        {
            total = total.checked_add(entry.size()).ok_or_else(|| {
                DomainError::InvalidOperation("解包镜像总大小超出支持范围。".to_string())
            })?;
        }
    }
    Ok(total)
}

fn map_firmware_extract_error(error: FirmwareExtractApplicationError) -> DomainError {
    match error {
        FirmwareExtractApplicationError::Canceled => {
            DomainError::UserCancelled("payload 固件提取已取消。".to_string())
        }
        error => DomainError::InvalidOperation(error.to_string()),
    }
}

fn map_preparation_io_error(error: io::Error) -> DomainError {
    if error.kind() == io::ErrorKind::Interrupted {
        DomainError::UserCancelled("线刷预检已取消。".to_string())
    } else {
        DomainError::InvalidOperation(format!("读取线刷固件失败：{error}"))
    }
}

fn map_embedded_asset_error(error: EmbeddedAssetError) -> DomainError {
    DomainError::InvalidOperation(format!("清除数据资源错误：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FirmwareExtractEntry;
    use std::sync::{Arc, Barrier};

    #[test]
    fn ota_download_cancellation_remains_a_domain_cancellation() {
        assert!(matches!(
            map_ota_download_error(OtaDownloadError::Cancelled),
            DomainError::UserCancelled(_)
        ));
    }

    #[test]
    fn ota_download_without_a_known_length_remains_an_operation_failure() {
        assert!(matches!(
            map_ota_download_error(OtaDownloadError::UnknownContentLength),
            DomainError::InvalidOperation(message) if message.contains("无法确定 OTA 包大小")
        ));
    }

    #[test]
    fn extraction_capacity_rejects_insufficient_space_before_unpacking_images() {
        let error = validate_extraction_capacity(11, 10)
            .expect_err("preflight must reject a staging drive that cannot hold all images");

        assert!(error.to_string().contains("磁盘空间不足"));
    }

    #[test]
    fn payload_output_size_rejects_negative_or_overflowing_metadata_before_staging() {
        let negative = checked_payload_output_size(&[FirmwareExtractEntry {
            id: "entry-boot".to_string(),
            name: "boot".to_string(),
            size_bytes: -1,
        }]);
        assert!(negative.is_err());

        let overflow = checked_payload_output_size(&[
            FirmwareExtractEntry {
                id: "entry-boot".to_string(),
                name: "boot".to_string(),
                size_bytes: i64::MAX,
            },
            FirmwareExtractEntry {
                id: "entry-vendor-boot".to_string(),
                name: "vendor_boot".to_string(),
                size_bytes: i64::MAX,
            },
            FirmwareExtractEntry {
                id: "entry-init-boot".to_string(),
                name: "init_boot".to_string(),
                size_bytes: i64::MAX,
            },
        ]);
        assert!(overflow.is_err());
    }

    #[test]
    fn concurrent_safe_flash_staging_roots_are_unique() {
        let service = Arc::new(SafeFlashService::new());
        let barrier = Arc::new(Barrier::new(64));
        let handles = (0..64)
            .map(|_| {
                let service = Arc::clone(&service);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    service.create_staging_root()
                })
            })
            .collect::<Vec<_>>();
        let roots = handles
            .into_iter()
            .map(|handle| handle.join().expect("staging root worker should complete"))
            .collect::<HashSet<_>>();

        assert_eq!(roots.len(), 64);
    }
}
