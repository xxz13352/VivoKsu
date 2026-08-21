//! Tauri commands for quick-flash plan expansion, command inspection, and execution.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::{task, time::sleep};
use tokio_util::sync::CancellationToken;

use nwflash_application::{
    result_to_domain_error, CommandSpec, OperationContext, QuickFlashService,
};
use nwflash_domain::{
    build_quick_flash_plan, FlashImageInfo, OperationKind, PartitionExecutionPlan,
    PartitionOperationKind, PartitionTask, PartitionTransportKind, QuickFlashOptions,
    QuickFlashPartition, QuickFlashRequest,
};
use nwflash_windows::bundled_platform_tool;
use nwflash_windows::process::{run_command_with_cancel, ProcessCommand, ProcessOutput};

use crate::{
    commands::device::{discover_current_device, DeviceRuntime},
    session_capabilities::{SessionCapabilityLease, SessionCapabilityScope},
    AppState,
};

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
pub struct QuickFlashPlanDto {
    pub serial: String,
    pub transport: String,
    pub operation: String,
    pub task_count: usize,
    pub commands: Vec<ProcessCommandDto>,
}

#[derive(Debug, Serialize)]
pub struct CommandExecutionResultDto {
    pub command_count: usize,
    pub executed_count: usize,
}

#[derive(Debug, Serialize)]
pub struct PreparedDualSlotConfirmationDto {
    pub task_count: usize,
    pub switch_slot_after_flash: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuickFlashPresetImageRequestDto {
    image_path: String,
    partition: QuickFlashPartition,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareArtifactConfirmationDto {
    pub partition: String,
    pub task_count: usize,
}

pub fn inspect_image_path(image_path: &str) -> Result<FlashImageInfo, String> {
    QuickFlashService::with_default_tools()
        .inspect_image(Path::new(image_path))
        .map_err(|error| error.to_string())
}

fn adb_root_resolution_tasks(plan: &PartitionExecutionPlan) -> Vec<Option<(String, String)>> {
    plan.tasks
        .iter()
        .map(|task| {
            matches!(plan.transport, PartitionTransportKind::AdbRoot)
                .then(|| (task.partition_name.clone(), task.device_path.clone()))
        })
        .collect()
}

async fn run_process_command(
    command: ProcessCommand,
    cancellation: Option<CancellationToken>,
) -> Result<ProcessOutput, nwflash_domain::DomainError> {
    let cancellation = cancellation.unwrap_or_default();
    let cancellation_for_command = cancellation.clone();
    task::spawn_blocking(move || {
        run_command_with_cancel(command, None, move || {
            cancellation_for_command.is_cancelled()
        })
    })
    .await
    .map_err(|error| nwflash_domain::DomainError::Internal(format!("命令执行调度失败：{error}")))?
}

fn command_process(command: &CommandSpec) -> ProcessCommand {
    ProcessCommand {
        program: command.program.clone(),
        args: command.args.clone(),
        working_directory: command.working_directory.clone(),
        environment: command.environment.clone(),
    }
}

fn partition_terminal_state(
    error: &nwflash_domain::DomainError,
) -> nwflash_domain::PartitionTaskState {
    if matches!(error, nwflash_domain::DomainError::UserCancelled(_)) {
        nwflash_domain::PartitionTaskState::Canceled
    } else {
        nwflash_domain::PartitionTaskState::Failed
    }
}

pub(crate) fn partition_terminal_updates(
    tasks: &[nwflash_domain::PartitionTask],
    failed_index: usize,
    error: &nwflash_domain::DomainError,
) -> Vec<nwflash_domain::PartitionTaskSnapshot> {
    let total = tasks.len();
    tasks
        .iter()
        .enumerate()
        .skip(failed_index)
        .map(|(index, task)| nwflash_domain::PartitionTaskSnapshot {
            partition_name: task.partition_name.clone(),
            state: if index == failed_index {
                partition_terminal_state(error)
            } else {
                nwflash_domain::PartitionTaskState::Canceled
            },
            overall_progress: failed_index as f64 / total as f64,
        })
        .collect()
}

pub(crate) async fn report_partition_terminal_updates(
    context: &OperationContext,
    tasks: &[PartitionTask],
    failed_index: usize,
    error: &nwflash_domain::DomainError,
) {
    for update in partition_terminal_updates(tasks, failed_index, error) {
        context
            .report_partition_task(update.partition_name, update.state, update.overall_progress)
            .await;
    }
}

#[tauri::command]
pub fn quick_flash_inspect_image(image_path: String) -> Result<FlashImageInfo, String> {
    inspect_image_path(&image_path)
}

pub fn build_boot_image_plan(
    device_runtime: &DeviceRuntime,
    image_path: &str,
) -> Result<QuickFlashPlanDto, String> {
    quick_flash_prepare_commands(build_boot_execution_plan(device_runtime, image_path)?)
}

pub fn build_boot_execution_plan(
    device_runtime: &DeviceRuntime,
    image_path: &str,
) -> Result<PartitionExecutionPlan, String> {
    build_preset_execution_plan(device_runtime, image_path, QuickFlashPartition::Boot)
}

pub fn build_preset_execution_plan(
    device_runtime: &DeviceRuntime,
    image_path: &str,
    partition: QuickFlashPartition,
) -> Result<PartitionExecutionPlan, String> {
    let image = inspect_image_path(image_path)?;
    let partition_name = partition.partition_name();
    Ok(PartitionExecutionPlan {
        serial: device_runtime.active_fastboot_serial()?,
        transport: PartitionTransportKind::Fastboot,
        operation: PartitionOperationKind::Write,
        tasks: vec![PartitionTask {
            partition_name: partition_name.to_string(),
            device_path: format!("/dev/block/by-name/{partition_name}"),
            image_path: Some(image.path),
            output_path: None,
            size_bytes: Some(image.size_bytes),
        }],
    })
}

fn build_batch_preset_execution_plan<F>(
    serial: &str,
    requests: &[QuickFlashRequest],
    options: &QuickFlashOptions,
    current_slot: Option<&str>,
    has_slot: F,
) -> Result<PreparedPresetExecutionPlan, String>
where
    F: Fn(&str) -> bool,
{
    let mut selected_partitions = HashSet::with_capacity(requests.len());
    for request in requests {
        if !selected_partitions.insert(request.partition.partition_name()) {
            return Err(format!(
                "分区 {} 不能在同一次快速刷写中重复选择。",
                request.partition.partition_name()
            ));
        }
    }

    let domain_plan = build_quick_flash_plan(requests, options, current_slot, has_slot)
        .map_err(|error| error.to_string())?;
    let image_sizes = requests
        .iter()
        .map(|request| (request.image.path.as_str(), request.image.size_bytes))
        .collect::<HashMap<_, _>>();
    let tasks = domain_plan
        .requests
        .into_iter()
        .map(|request| {
            let size_bytes = image_sizes
                .get(request.image_path.as_str())
                .copied()
                .ok_or_else(|| "快速刷写计划中的镜像已失效，请重新确认。".to_string())?;
            Ok(PartitionTask {
                device_path: format!("/dev/block/by-name/{}", request.partition_name),
                partition_name: request.partition_name,
                image_path: Some(request.image_path),
                output_path: None,
                size_bytes: Some(size_bytes),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(PreparedPresetExecutionPlan {
        plan: PartitionExecutionPlan {
            serial: serial.to_string(),
            transport: PartitionTransportKind::Fastboot,
            operation: PartitionOperationKind::Write,
            tasks,
        },
        switch_to_slot: domain_plan.switch_to_slot,
    })
}

fn inspect_batch_preset_requests(
    requests: Vec<QuickFlashPresetImageRequestDto>,
) -> Result<Vec<QuickFlashRequest>, String> {
    if requests.is_empty() {
        return Err("快速刷写至少需要选择一个镜像。".to_string());
    }

    let mut selected_partitions = HashSet::with_capacity(requests.len());
    requests
        .into_iter()
        .map(|request| {
            if !selected_partitions.insert(request.partition.partition_name()) {
                return Err(format!(
                    "分区 {} 不能在同一次快速刷写中重复选择。",
                    request.partition.partition_name()
                ));
            }
            Ok(QuickFlashRequest {
                partition: request.partition,
                image: inspect_image_path(&request.image_path)?,
            })
        })
        .collect()
}

fn parse_fastboot_device_serials(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?;
            let state = fields.next()?;
            (state.eq_ignore_ascii_case("fastboot") && !serial.is_empty())
                .then(|| serial.to_string())
        })
        .collect()
}

fn select_waiting_fastboot_device(
    serials: &[String],
) -> Result<Option<String>, nwflash_domain::DomainError> {
    match serials {
        [] => Ok(None),
        [serial] => Ok(Some(serial.clone())),
        _ => Err(nwflash_domain::DomainError::DeviceUnavailable(
            "检测到多个 Fastboot 设备，已取消快速刷写。请仅连接目标设备后重试。".to_string(),
        )),
    }
}

fn record_discovered_fastboot_device(device_runtime: &DeviceRuntime, serial: &str) {
    device_runtime.apply_snapshot(
        nwflash_domain::DeviceSnapshot {
            connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
            serial: serial.to_string(),
            connection_label: "Fastboot 已连接".to_string(),
            model: "--".to_string(),
            android_version: "--".to_string(),
            battery_level: "--".to_string(),
        },
        false,
        nwflash_domain::DeviceRefreshMode::Manual,
    );
}

async fn run_fastboot_probe(
    command: ProcessCommand,
    cancellation: CancellationToken,
) -> Result<ProcessOutput, nwflash_domain::DomainError> {
    let cancellation_for_command = cancellation.clone();
    task::spawn_blocking(move || {
        run_command_with_cancel(command, None, move || {
            cancellation_for_command.is_cancelled()
        })
    })
    .await
    .map_err(|error| {
        nwflash_domain::DomainError::Internal(format!("Fastboot 检测调度失败：{error}"))
    })?
}

type FastbootProbeFuture = Pin<
    Box<
        dyn Future<Output = Result<ProcessOutput, nwflash_domain::DomainError>> + Send + 'static,
    >,
>;
type FastbootProbe =
    dyn Fn(ProcessCommand, CancellationToken) -> FastbootProbeFuture + Send + Sync;
type DeviceDiscoveryFuture = Pin<
    Box<
        dyn Future<Output = Result<nwflash_domain::DeviceSnapshot, String>> + Send + 'static,
    >,
>;
type DeviceDiscovery = dyn Fn() -> DeviceDiscoveryFuture + Send + Sync;

fn production_fastboot_probe(
    command: ProcessCommand,
    cancellation: CancellationToken,
) -> FastbootProbeFuture {
    Box::pin(run_fastboot_probe(command, cancellation))
}

fn production_device_discovery() -> DeviceDiscoveryFuture {
    Box::pin(discover_current_device())
}

async fn read_fastboot_variable(
    serial: &str,
    variable: &str,
    cancellation: CancellationToken,
) -> Result<String, nwflash_domain::DomainError> {
    read_fastboot_variable_with_probe(
        serial,
        variable,
        cancellation,
        &production_fastboot_probe,
    )
    .await
}

async fn read_fastboot_variable_with_probe(
    serial: &str,
    variable: &str,
    cancellation: CancellationToken,
    fastboot_probe: &FastbootProbe,
) -> Result<String, nwflash_domain::DomainError> {
    let command = ProcessCommand::new(
        bundled_platform_tool("fastboot.exe"),
        [
            "-s".to_string(),
            serial.to_string(),
            "getvar".to_string(),
            variable.to_string(),
        ],
    );
    let output = fastboot_probe(command, cancellation).await?;
    if output.exit_code != 0 {
        return Err(nwflash_domain::DomainError::ExternalTool(format!(
            "读取 Fastboot 变量 {variable} 失败，退出码 {}。",
            output.exit_code
        )));
    }
    Ok(format!("{}\n{}", output.stdout, output.stderr))
}

async fn resolve_fastbootd_serial(
    device_runtime: DeviceRuntime,
    wait_for_device: bool,
    context: &OperationContext,
    cancellation: CancellationToken,
) -> Result<String, nwflash_domain::DomainError> {
    resolve_fastbootd_serial_with_probe(
        device_runtime,
        wait_for_device,
        context,
        cancellation,
        &production_fastboot_probe,
    )
    .await
}

async fn resolve_fastbootd_serial_with_probe(
    device_runtime: DeviceRuntime,
    wait_for_device: bool,
    context: &OperationContext,
    cancellation: CancellationToken,
    fastboot_probe: &FastbootProbe,
) -> Result<String, nwflash_domain::DomainError> {
    loop {
        if cancellation.is_cancelled() {
            return Err(nwflash_domain::DomainError::UserCancelled(
                "运行被用户取消".to_string(),
            ));
        }
        context.report_stage("正在等待 fastbootd 设备");
        let output = fastboot_probe(
            ProcessCommand::new(
                bundled_platform_tool("fastboot.exe"),
                ["devices".to_string()],
            ),
            cancellation.clone(),
        )
        .await?;
        if output.exit_code != 0 {
            return Err(nwflash_domain::DomainError::ExternalTool(format!(
                "检测 Fastboot 设备失败，退出码 {}。",
                output.exit_code
            )));
        }
        let Some(serial) =
            select_waiting_fastboot_device(&parse_fastboot_device_serials(&output.stdout))?
        else {
            if !wait_for_device {
                return Err(nwflash_domain::DomainError::DeviceUnavailable(
                    "未检测到可刷写的 fastbootd 设备。".to_string(),
                ));
            }
            sleep(Duration::from_secs(1)).await;
            continue;
        };

        if let Ok(output) = read_fastboot_variable_with_probe(
            &serial,
            "is-userspace",
            cancellation.clone(),
            fastboot_probe,
        )
        .await
        {
            if is_true_fastboot_variable(&output, "is-userspace") {
                record_discovered_fastboot_device(&device_runtime, &serial);
                return Ok(serial);
            }
        }

        if !wait_for_device {
            return Err(nwflash_domain::DomainError::DeviceUnavailable(
                "未检测到可刷写的 fastbootd 设备。".to_string(),
            ));
        }
        sleep(Duration::from_secs(1)).await;
    }
}

async fn prepare_batch_preset_execution_plan(
    device_runtime: DeviceRuntime,
    requests: Vec<QuickFlashRequest>,
    options: QuickFlashOptions,
    context: &OperationContext,
    cancellation: CancellationToken,
) -> Result<PreparedPresetExecutionPlan, nwflash_domain::DomainError> {
    let serial = resolve_fastbootd_serial(
        device_runtime,
        options.wait_for_device,
        context,
        cancellation.clone(),
    )
    .await?;
    let mut has_slots = HashMap::new();
    let current_slot = if options.flash_both_slots {
        for request in &requests {
            let variable = format!("has-slot:{}", request.partition.partition_name());
            context.report_stage(format!(
                "检查 {} 的 A/B 双槽能力",
                request.partition.partition_name()
            ));
            let output =
                read_fastboot_variable(&serial, &variable, cancellation.clone()).await?;
            has_slots.insert(
                request.partition.partition_name().to_string(),
                is_true_fastboot_variable(&output, &variable),
            );
        }
        if options.switch_slot_after_flash {
            Some(read_fastboot_variable(&serial, "current-slot", cancellation).await?)
        } else {
            None
        }
    } else {
        None
    };

    build_batch_preset_execution_plan(
        &serial,
        &requests,
        &options,
        current_slot.as_deref(),
        |partition| has_slots.get(partition).copied().unwrap_or(false),
    )
    .map_err(nwflash_domain::DomainError::InvalidOperation)
}

fn build_post_flash_commands(serial: &str, auto_reboot: bool) -> Vec<ProcessCommand> {
    if auto_reboot {
        vec![ProcessCommand::new(
            bundled_platform_tool("fastboot.exe"),
            ["-s".to_string(), serial.to_string(), "reboot".to_string()],
        )]
    } else {
        Vec::new()
    }
}

fn build_post_flash_slot_switch_command(serial: &str, slot: &str) -> ProcessCommand {
    ProcessCommand::new(
        bundled_platform_tool("fastboot.exe"),
        [
            "-s".to_string(),
            serial.to_string(),
            "set_active".to_string(),
            slot.to_string(),
        ],
    )
}

async fn build_post_flash_slot_switch_for_current_device(
    device_runtime: &DeviceRuntime,
    slot: &str,
    context: &OperationContext,
    cancellation: CancellationToken,
) -> Result<ProcessCommand, nwflash_domain::DomainError> {
    build_post_flash_slot_switch_for_current_device_with_fastboot_probe(
        device_runtime,
        slot,
        context,
        cancellation,
        &production_fastboot_probe,
    )
    .await
}

async fn build_post_flash_slot_switch_for_current_device_with_fastboot_probe(
    device_runtime: &DeviceRuntime,
    slot: &str,
    context: &OperationContext,
    cancellation: CancellationToken,
    fastboot_probe: &FastbootProbe,
) -> Result<ProcessCommand, nwflash_domain::DomainError> {
    let serial = resolve_fastbootd_serial_with_probe(
        device_runtime.clone(),
        false,
        context,
        cancellation,
        fastboot_probe,
    )
    .await?;
    Ok(build_post_flash_slot_switch_command(&serial, slot))
}

async fn build_post_flash_reboot_for_current_device(
    device_runtime: &DeviceRuntime,
    context: &OperationContext,
    cancellation: CancellationToken,
) -> Result<ProcessCommand, nwflash_domain::DomainError> {
    build_post_flash_reboot_for_current_device_with_fastboot_probe(
        device_runtime,
        context,
        cancellation,
        &production_fastboot_probe,
    )
    .await
}

async fn build_post_flash_reboot_for_current_device_with_fastboot_probe(
    device_runtime: &DeviceRuntime,
    context: &OperationContext,
    cancellation: CancellationToken,
    fastboot_probe: &FastbootProbe,
) -> Result<ProcessCommand, nwflash_domain::DomainError> {
    let serial = resolve_fastbootd_serial_with_probe(
        device_runtime.clone(),
        false,
        context,
        cancellation,
        fastboot_probe,
    )
    .await?;
    Ok(build_post_flash_commands(&serial, true)
        .into_iter()
        .next()
        .expect("auto reboot should produce a command"))
}

pub fn build_firmware_artifact_execution_plan(
    artifacts: &crate::commands::firmware::FirmwareArtifactRuntime,
    device_runtime: &DeviceRuntime,
    artifact_id: &str,
) -> Result<PartitionExecutionPlan, String> {
    let artifact = artifacts.get(artifact_id)?;
    if !Path::new(&artifact.image.path).starts_with(&artifact.staging_root) {
        return Err("固件提取结果无效，请重新提取。".to_string());
    }
    build_preset_execution_plan(device_runtime, &artifact.image.path, artifact.partition)
}

pub fn prepare_firmware_artifact_confirmation(
    artifacts: &crate::commands::firmware::FirmwareArtifactRuntime,
    device_runtime: &DeviceRuntime,
    prepared_runtime: &PreparedFirmwareArtifactRuntime,
    artifact_id: &str,
) -> Result<FirmwareArtifactConfirmationDto, String> {
    let lease = prepared_runtime.capture_lease()?;
    let plan = build_firmware_artifact_execution_plan(artifacts, device_runtime, artifact_id)?;
    let partition = plan
        .tasks
        .first()
        .map(|task| task.partition_name.clone())
        .ok_or_else(|| "固件提取结果没有可刷写分区。".to_string())?;
    prepared_runtime.replace_with_lease(lease, artifact_id.to_string(), plan.clone())?;
    Ok(FirmwareArtifactConfirmationDto {
        partition,
        task_count: plan.tasks.len(),
    })
}

pub fn build_dual_slot_preflight_commands(
    device_runtime: &DeviceRuntime,
    partition: QuickFlashPartition,
    switch_slot_after_flash: bool,
) -> Result<Vec<ProcessCommand>, String> {
    let serial = device_runtime.active_fastboot_serial()?;
    let mut commands = vec![ProcessCommand::new(
        bundled_platform_tool("fastboot.exe"),
        [
            "-s".to_string(),
            serial.clone(),
            "getvar".to_string(),
            format!("has-slot:{}", partition.partition_name()),
        ],
    )];
    if switch_slot_after_flash {
        commands.push(ProcessCommand::new(
            bundled_platform_tool("fastboot.exe"),
            [
                "-s".to_string(),
                serial,
                "getvar".to_string(),
                "current-slot".to_string(),
            ],
        ));
    }
    Ok(commands)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedPresetExecutionPlan {
    pub plan: PartitionExecutionPlan,
    pub switch_to_slot: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedDualSlotEntry {
    epoch: u64,
    prepared: PreparedPresetExecutionPlan,
}

#[derive(Clone)]
pub struct PreparedDualSlotRuntime {
    scope: Arc<SessionCapabilityScope>,
    prepared: Arc<Mutex<Option<PreparedDualSlotEntry>>>,
}

impl PreparedDualSlotRuntime {
    pub fn new() -> Self {
        Self::with_scope(Arc::new(SessionCapabilityScope::new()))
    }

    pub(crate) fn with_scope(scope: Arc<SessionCapabilityScope>) -> Self {
        Self {
            scope,
            prepared: Arc::new(Mutex::new(None)),
        }
    }

    fn capture_lease(&self) -> Result<SessionCapabilityLease, String> {
        self.scope
            .capture()
            .map_err(|_| "当前会话已失效，请重新完成双槽刷写预检。".to_string())
    }

    pub fn replace(&self, prepared: PreparedPresetExecutionPlan) -> Result<(), String> {
        let lease = self.capture_lease()?;
        self.replace_with_lease(lease, prepared)
    }

    fn replace_with_lease(
        &self,
        lease: SessionCapabilityLease,
        prepared: PreparedPresetExecutionPlan,
    ) -> Result<(), String> {
        self.scope
            .commit(lease, || {
                *self
                    .prepared
                    .lock()
                    .expect("prepared dual-slot plan lock should not be poisoned") =
                    Some(PreparedDualSlotEntry {
                        epoch: lease.epoch,
                        prepared,
                    });
            })
            .map_err(|_| "当前会话已失效，请重新完成双槽刷写预检。".to_string())
    }

    pub fn take(&self) -> Result<PreparedPresetExecutionPlan, String> {
        let lease = self.capture_lease()?;
        self.scope
            .commit(lease, || {
                let mut prepared = self
                    .prepared
                    .lock()
                    .expect("prepared dual-slot plan lock should not be poisoned");
                match prepared.take() {
                    Some(entry) if entry.epoch == lease.epoch => Ok(entry.prepared),
                    Some(entry) => {
                        *prepared = Some(entry);
                        Err("双槽刷写预检已失效，请重新确认计划。".to_string())
                    }
                    None => Err("请先完成双槽刷写预检并确认计划。".to_string()),
                }
            })
            .map_err(|_| "当前会话已失效，请重新完成双槽刷写预检。".to_string())?
    }

    pub(crate) fn clear(&self) {
        *self
            .prepared
            .lock()
            .expect("prepared dual-slot plan lock should not be poisoned") = None;
    }
}

impl Default for PreparedDualSlotRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// A Rust-resident, single-use authorization created by firmware artifact preflight.
#[derive(Debug, Clone)]
struct PreparedFirmwareArtifactEntry {
    epoch: u64,
    artifact_id: String,
    plan: PartitionExecutionPlan,
}

#[derive(Clone)]
pub struct PreparedFirmwareArtifactRuntime {
    scope: Arc<SessionCapabilityScope>,
    prepared: Arc<Mutex<Option<PreparedFirmwareArtifactEntry>>>,
}

impl PreparedFirmwareArtifactRuntime {
    pub fn new() -> Self {
        Self::with_scope(Arc::new(SessionCapabilityScope::new()))
    }

    pub(crate) fn with_scope(scope: Arc<SessionCapabilityScope>) -> Self {
        Self {
            scope,
            prepared: Arc::new(Mutex::new(None)),
        }
    }

    fn capture_lease(&self) -> Result<SessionCapabilityLease, String> {
        self.scope
            .capture()
            .map_err(|_| "当前会话已失效，请重新完成固件刷写预检。".to_string())
    }

    pub fn replace(&self, artifact_id: String, plan: PartitionExecutionPlan) -> Result<(), String> {
        let lease = self.capture_lease()?;
        self.replace_with_lease(lease, artifact_id, plan)
    }

    fn replace_with_lease(
        &self,
        lease: SessionCapabilityLease,
        artifact_id: String,
        plan: PartitionExecutionPlan,
    ) -> Result<(), String> {
        self.scope
            .commit(lease, || {
                *self
                    .prepared
                    .lock()
                    .expect("prepared firmware artifact lock should not be poisoned") =
                    Some(PreparedFirmwareArtifactEntry {
                        epoch: lease.epoch,
                        artifact_id,
                        plan,
                    });
            })
            .map_err(|_| "当前会话已失效，请重新完成固件刷写预检。".to_string())
    }

    pub fn take(&self, artifact_id: &str) -> Result<PartitionExecutionPlan, String> {
        let lease = self.capture_lease()?;
        self.scope
            .commit(lease, || {
                let mut prepared = self
                    .prepared
                    .lock()
                    .expect("prepared firmware artifact lock should not be poisoned");
                match prepared.take() {
                    Some(entry)
                        if entry.artifact_id == artifact_id && entry.epoch == lease.epoch =>
                    {
                        Ok(entry.plan)
                    }
                    Some(entry) => {
                        *prepared = Some(entry);
                        Err("固件刷写预检已失效，请重新确认刷写。".to_string())
                    }
                    None => Err("请先完成固件刷写预检并确认刷写。".to_string()),
                }
            })
            .map_err(|_| "当前会话已失效，请重新完成固件刷写预检。".to_string())?
    }

    pub(crate) fn clear(&self) {
        *self
            .prepared
            .lock()
            .expect("prepared firmware artifact lock should not be poisoned") = None;
    }
}

impl Default for PreparedFirmwareArtifactRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub fn build_verified_dual_slot_execution_plan(
    device_runtime: &DeviceRuntime,
    image_path: &str,
    partition: QuickFlashPartition,
    has_slot_output: &str,
    current_slot_output: Option<&str>,
    switch_slot_after_flash: bool,
) -> Result<PreparedPresetExecutionPlan, String> {
    let has_slot_variable = format!("has-slot:{}", partition.partition_name());
    if !is_true_fastboot_variable(has_slot_output, &has_slot_variable) {
        return Err(format!(
            "设备分区 {} 不支持 A/B 双槽刷写。",
            partition.partition_name()
        ));
    }

    let single = build_preset_execution_plan(device_runtime, image_path, partition)?;
    let source_task = single
        .tasks
        .first()
        .cloned()
        .ok_or_else(|| "快速刷写计划为空。".to_string())?;
    let mut tasks = Vec::with_capacity(2);
    for slot in ["a", "b"] {
        let partition_name = format!("{}_{}", partition.partition_name(), slot);
        tasks.push(PartitionTask {
            partition_name: partition_name.clone(),
            device_path: format!("/dev/block/by-name/{partition_name}"),
            ..source_task.clone()
        });
    }

    let switch_to_slot = if switch_slot_after_flash {
        Some(
            opposite_slot(normalize_current_slot(
                current_slot_output.ok_or_else(|| "无法确定设备当前活动槽位。".to_string())?,
            )?)
            .to_string(),
        )
    } else {
        None
    };

    Ok(PreparedPresetExecutionPlan {
        plan: PartitionExecutionPlan { tasks, ..single },
        switch_to_slot,
    })
}

async fn run_dual_slot_preflight(
    coordinator: nwflash_application::OperationCoordinator,
    device_runtime: DeviceRuntime,
    partition: QuickFlashPartition,
    switch_slot_after_flash: bool,
) -> Result<Vec<String>, String> {
    let outputs = Arc::new(Mutex::new(Vec::with_capacity(
        1 + usize::from(switch_slot_after_flash),
    )));
    let outputs_for_operation = outputs.clone();
    coordinator
        .run_async(
            OperationKind::Discovering,
            "检查双槽刷写条件",
            move |context, cancellation| async move {
                let commands = build_dual_slot_preflight_commands(
                    &device_runtime,
                    partition,
                    switch_slot_after_flash,
                )
                .map_err(nwflash_domain::DomainError::DeviceUnavailable)?;
                for command in commands {
                    context.report_stage("读取 Fastboot 双槽能力");
                    let cancellation_for_command = cancellation.clone();
                    let output = task::spawn_blocking(move || {
                        run_command_with_cancel(command, None, move || {
                            cancellation_for_command.is_cancelled()
                        })
                    })
                    .await
                    .map_err(|error| {
                        nwflash_domain::DomainError::Internal(format!("双槽预检调度失败：{error}"))
                    })??;
                    if output.exit_code != 0 {
                        return Err(nwflash_domain::DomainError::ExternalTool(format!(
                            "读取 Fastboot 双槽信息失败，退出码 {}：{}",
                            output.exit_code, output.stderr
                        )));
                    }
                    outputs_for_operation
                        .lock()
                        .expect("dual-slot preflight output lock should not be poisoned")
                        .push(format!("{}\n{}", output.stdout, output.stderr));
                }
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let outputs = outputs
        .lock()
        .expect("dual-slot preflight output lock should not be poisoned")
        .clone();
    Ok(outputs)
}

fn is_true_fastboot_variable(output: &str, variable: &str) -> bool {
    let prefix = format!("{variable}:");
    output
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("(bootloader)").unwrap_or(line).trim();
            line.get(..prefix.len())
                .filter(|candidate| candidate.eq_ignore_ascii_case(&prefix))
                .map(|_| line[prefix.len()..].trim())
                .filter(|value| !value.is_empty())
        })
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "yes" | "true" | "1" | "on"
            )
        })
}

fn normalize_current_slot(output: &str) -> Result<&str, String> {
    let value = output
        .lines()
        .find_map(|line| line.rsplit_once(':').map(|(_, value)| value.trim()))
        .unwrap_or_else(|| output.trim());
    match value {
        "a" | "_a" => Ok("a"),
        "b" | "_b" => Ok("b"),
        _ => Err("无法确定设备当前活动槽位。".to_string()),
    }
}

fn opposite_slot(slot: &str) -> &'static str {
    if slot == "a" {
        "b"
    } else {
        "a"
    }
}

#[tauri::command]
pub fn quick_flash_prepare_boot_image(
    state: State<'_, AppState>,
    image_path: String,
) -> Result<QuickFlashPlanDto, String> {
    build_boot_image_plan(&state.device_runtime, &image_path)
}

#[tauri::command]
pub fn quick_flash_prepare_preset_image(
    state: State<'_, AppState>,
    image_path: String,
    partition: QuickFlashPartition,
    _auto_reboot: Option<bool>,
    _wait_for_device: Option<bool>,
) -> Result<QuickFlashPlanDto, String> {
    quick_flash_prepare_commands(build_preset_execution_plan(
        &state.device_runtime,
        &image_path,
        partition,
    )?)
}

#[tauri::command]
pub fn quick_flash_prepare_firmware_artifact(
    state: State<'_, AppState>,
    artifact_id: String,
) -> Result<FirmwareArtifactConfirmationDto, String> {
    prepare_firmware_artifact_confirmation(
        &state.firmware_artifacts,
        &state.device_runtime,
        &state.prepared_firmware_artifact,
        &artifact_id,
    )
}

#[tauri::command]
pub async fn quick_flash_prepare_dual_slot_preset_image(
    state: State<'_, AppState>,
    image_path: String,
    partition: QuickFlashPartition,
    switch_slot_after_flash: bool,
) -> Result<PreparedDualSlotConfirmationDto, String> {
    let lease = state.prepared_dual_slot.capture_lease()?;
    let outputs = run_dual_slot_preflight(
        state.operation_coordinator.clone(),
        state.device_runtime.clone(),
        partition,
        switch_slot_after_flash,
    )
    .await?;
    let prepared = build_verified_dual_slot_execution_plan(
        &state.device_runtime,
        &image_path,
        partition,
        outputs.first().map(String::as_str).unwrap_or_default(),
        outputs.get(1).map(String::as_str),
        switch_slot_after_flash,
    )?;
    let task_count = prepared.plan.tasks.len();
    let will_switch_slot = prepared.switch_to_slot.is_some();
    state
        .prepared_dual_slot
        .replace_with_lease(lease, prepared)?;
    Ok(PreparedDualSlotConfirmationDto {
        task_count,
        switch_slot_after_flash: will_switch_slot,
    })
}

#[tauri::command]
pub async fn quick_flash_execute_boot_image(
    state: State<'_, AppState>,
    image_path: String,
) -> Result<CommandExecutionResultDto, String> {
    quick_flash_execute_preset_images(
        state,
        vec![QuickFlashPresetImageRequestDto {
            image_path,
            partition: QuickFlashPartition::Boot,
        }],
        true,
        true,
        false,
        false,
    )
    .await
}

#[tauri::command]
pub async fn quick_flash_execute_preset_image(
    state: State<'_, AppState>,
    image_path: String,
    partition: QuickFlashPartition,
    auto_reboot: Option<bool>,
    wait_for_device: Option<bool>,
) -> Result<CommandExecutionResultDto, String> {
    quick_flash_execute_preset_images(
        state,
        vec![QuickFlashPresetImageRequestDto {
            image_path,
            partition,
        }],
        auto_reboot.unwrap_or(true),
        wait_for_device.unwrap_or(true),
        false,
        false,
    )
    .await
}

#[tauri::command]
pub async fn quick_flash_execute_preset_images(
    state: State<'_, AppState>,
    requests: Vec<QuickFlashPresetImageRequestDto>,
    auto_reboot: bool,
    wait_for_device: bool,
    flash_both_slots: bool,
    switch_slot_after_flash: bool,
) -> Result<CommandExecutionResultDto, String> {
    let requests = inspect_batch_preset_requests(requests)?;
    let options = QuickFlashOptions {
        target: nwflash_domain::FastbootTarget::Fastbootd,
        wait_for_device,
        flash_both_slots,
        switch_slot_after_flash,
        auto_reboot,
    };
    let prepared = Arc::new(Mutex::new(None));
    let prepared_for_operation = prepared.clone();
    let device_runtime = state.device_runtime.clone();
    let options_for_operation = options.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Discovering,
            "检查快速刷写条件",
            move |context, cancellation| async move {
                let execution = prepare_batch_preset_execution_plan(
                    device_runtime,
                    requests,
                    options_for_operation,
                    &context,
                    cancellation,
                )
                .await?;
                *prepared_for_operation.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal("快速刷写预检结果锁不可用。".to_string())
                })? = Some(execution);
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let prepared = prepared
        .lock()
        .map_err(|_| "快速刷写预检结果锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "快速刷写预检未产生执行计划。".to_string())?;
    quick_flash_execute_commands_with_post_actions(
        state,
        prepared.plan,
        options.auto_reboot,
        prepared.switch_to_slot,
    )
    .await
}

#[tauri::command]
pub async fn quick_flash_execute_firmware_artifact(
    state: State<'_, AppState>,
    artifact_id: String,
) -> Result<CommandExecutionResultDto, String> {
    quick_flash_execute_firmware_artifact_inner(&state, artifact_id).await
}

async fn quick_flash_execute_firmware_artifact_inner(
    state: &AppState,
    artifact_id: String,
) -> Result<CommandExecutionResultDto, String> {
    let firmware_artifacts = state.firmware_artifacts.clone();
    let prepared_firmware_artifact = state.prepared_firmware_artifact.clone();
    quick_flash_execute_with_plan_provider(state, "快速刷写固件".to_string(), move || {
        // Reject a capability if staging a newer artifact invalidated its opaque ID.
        firmware_artifacts.get(&artifact_id)?;
        let plan = prepared_firmware_artifact.take(&artifact_id)?;
        Ok(QuickFlashExecutionRequest {
            plan,
            auto_reboot: true,
            switch_to_slot: None,
        })
    })
    .await
}

#[tauri::command]
pub async fn quick_flash_execute_prepared_dual_slot_preset(
    state: State<'_, AppState>,
) -> Result<CommandExecutionResultDto, String> {
    quick_flash_execute_prepared_dual_slot_preset_inner(&state).await
}

async fn quick_flash_execute_prepared_dual_slot_preset_inner(
    state: &AppState,
) -> Result<CommandExecutionResultDto, String> {
    let prepared_dual_slot = state.prepared_dual_slot.clone();
    quick_flash_execute_with_plan_provider(state, "快速双槽刷写".to_string(), move || {
        let prepared = prepared_dual_slot.take()?;
        Ok(QuickFlashExecutionRequest {
            plan: prepared.plan,
            auto_reboot: false,
            switch_to_slot: prepared.switch_to_slot,
        })
    })
    .await
}

pub fn quick_flash_prepare_commands(
    plan: PartitionExecutionPlan,
) -> Result<QuickFlashPlanDto, String> {
    let service = QuickFlashService::with_default_tools();
    let commands = service
        .build_commands(&plan)
        .map_err(|error| error.to_string())?;
    let command_dtos = commands.into_iter().map(ProcessCommandDto::from).collect();

    Ok(QuickFlashPlanDto {
        serial: plan.serial,
        transport: format!("{:?}", plan.transport),
        operation: format!("{:?}", plan.operation),
        task_count: plan.tasks.len(),
        commands: command_dtos,
    })
}

pub async fn quick_flash_execute_commands(
    state: State<'_, AppState>,
    plan: PartitionExecutionPlan,
    auto_reboot: bool,
) -> Result<CommandExecutionResultDto, String> {
    quick_flash_execute_commands_with_post_actions(state, plan, auto_reboot, None).await
}

/// Resolves the sole live device in the plan's transport mode and overwrites
/// the plan's transient preview serial immediately before command construction.
async fn resolve_execution_plan(
    device_runtime: &DeviceRuntime,
    plan: PartitionExecutionPlan,
    context: &OperationContext,
    cancellation: CancellationToken,
) -> Result<PartitionExecutionPlan, nwflash_domain::DomainError> {
    resolve_execution_plan_with_discovery(
        device_runtime,
        plan,
        context,
        cancellation,
        &production_fastboot_probe,
        &production_device_discovery,
    )
    .await
}

#[cfg(test)]
async fn resolve_execution_plan_with_fastboot_probe(
    device_runtime: &DeviceRuntime,
    plan: PartitionExecutionPlan,
    context: &OperationContext,
    cancellation: CancellationToken,
    fastboot_probe: &FastbootProbe,
) -> Result<PartitionExecutionPlan, nwflash_domain::DomainError> {
    resolve_execution_plan_with_discovery(
        device_runtime,
        plan,
        context,
        cancellation,
        fastboot_probe,
        &production_device_discovery,
    )
    .await
}

async fn resolve_execution_plan_with_discovery(
    device_runtime: &DeviceRuntime,
    plan: PartitionExecutionPlan,
    context: &OperationContext,
    cancellation: CancellationToken,
    fastboot_probe: &FastbootProbe,
    device_discovery: &DeviceDiscovery,
) -> Result<PartitionExecutionPlan, nwflash_domain::DomainError> {
    let current_serial = match plan.transport {
        PartitionTransportKind::Fastboot => {
            resolve_fastbootd_serial_with_probe(
                device_runtime.clone(),
                false,
                context,
                cancellation,
                fastboot_probe,
            )
            .await?
        }
        PartitionTransportKind::AdbRoot => {
            if cancellation.is_cancelled() {
                return Err(nwflash_domain::DomainError::UserCancelled(
                    "运行被用户取消".to_string(),
                ));
            }
            context.report_stage("正在检测 ADB 设备");
            let snapshot = device_discovery()
                .await
                .map_err(nwflash_domain::DomainError::DeviceUnavailable)?;
            let is_adb_target = snapshot.connection_state
                == nwflash_domain::DeviceConnectionState::AdbConnected
                && !snapshot.serial.trim().is_empty()
                && snapshot.serial != "--";
            let serial = snapshot.serial.clone();
            device_runtime.apply_snapshot(
                snapshot,
                false,
                nwflash_domain::DeviceRefreshMode::Manual,
            );
            if cancellation.is_cancelled() {
                return Err(nwflash_domain::DomainError::UserCancelled(
                    "运行被用户取消".to_string(),
                ));
            }
            if !is_adb_target {
                return Err(nwflash_domain::DomainError::DeviceUnavailable(
                    "当前没有可用的 ADB 设备。".to_string(),
                ));
            }
            serial
        }
        PartitionTransportKind::Automatic => {
            return Err(nwflash_domain::DomainError::InvalidOperation(
                "执行计划必须使用已解析的设备通道。".to_string(),
            ));
        }
    };
    QuickFlashService::with_default_tools().retarget_execution_plan(&plan, &current_serial)
}

async fn quick_flash_execute_commands_with_post_actions(
    state: State<'_, AppState>,
    plan: PartitionExecutionPlan,
    auto_reboot: bool,
    switch_to_slot: Option<String>,
) -> Result<CommandExecutionResultDto, String> {
    quick_flash_execute_commands_with_post_actions_inner(&state, plan, auto_reboot, switch_to_slot)
        .await
}

async fn quick_flash_execute_commands_with_post_actions_inner(
    state: &AppState,
    plan: PartitionExecutionPlan,
    auto_reboot: bool,
    switch_to_slot: Option<String>,
) -> Result<CommandExecutionResultDto, String> {
    let transport = format!("{:?}", plan.transport);
    let title = format!("快速刷写{transport}({}项)", plan.tasks.len());
    quick_flash_execute_with_plan_provider(state, title, move || {
        Ok(QuickFlashExecutionRequest {
            plan,
            auto_reboot,
            switch_to_slot,
        })
    })
    .await
}

pub(super) struct QuickFlashExecutionRequest {
    pub(super) plan: PartitionExecutionPlan,
    pub(super) auto_reboot: bool,
    pub(super) switch_to_slot: Option<String>,
}

pub(super) async fn quick_flash_execute_with_plan_provider<P>(
    state: &AppState,
    title: String,
    plan_provider: P,
) -> Result<CommandExecutionResultDto, String>
where
    P: FnOnce() -> Result<QuickFlashExecutionRequest, String> + Send,
{
    let device_runtime = state.device_runtime.clone();
    let execution_counts = Arc::new(Mutex::new(None));
    let execution_counts_for_run = execution_counts.clone();

    state
        .operation_coordinator
        .run_async(
            OperationKind::Flashing,
            title,
            move |context, cancellation| async move {
                let QuickFlashExecutionRequest {
                    plan,
                    auto_reboot,
                    switch_to_slot,
                } = plan_provider().map_err(nwflash_domain::DomainError::InvalidOperation)?;
                let plan = resolve_execution_plan(
                    &device_runtime,
                    plan,
                    &context,
                    cancellation.clone(),
                )
                .await?;
                let service = QuickFlashService::with_default_tools();
                let task_commands = service.build_task_commands(&plan)?;
                let resolution_tasks = adb_root_resolution_tasks(&plan);
                let task_total = task_commands.len();
                let command_total = task_commands
                    .iter()
                    .map(|task| {
                        task.commands.len() + usize::from(task.cleanup_command.is_some())
                    })
                    .sum::<usize>();
                let has_slot_switch = switch_to_slot.is_some();

                for (index, task_commands) in task_commands.iter().enumerate() {
                    let resolution_task = resolution_tasks[index].clone();
                    let partition_name = task_commands.partition_name.clone();

                    if cancellation.is_cancelled() {
                        let error = nwflash_domain::DomainError::UserCancelled(
                            "运行被用户取消".to_string(),
                        );
                        report_partition_terminal_updates(&context, &plan.tasks, index, &error)
                            .await;
                        return Err(error);
                    }

                    let current = index + 1;
                    context
                        .report_partition_task(
                            partition_name.clone(),
                            nwflash_domain::PartitionTaskState::Running,
                            index as f64 / task_total as f64,
                        )
                        .await;

                    let task_result: Result<(), nwflash_domain::DomainError> = async {
                        if let Some((partition_name, expected_path)) = resolution_task {
                            context.report_stage(format!("校验 ADB Root 分区 {partition_name}"));
                            let resolution_command = crate::commands::partitions::build_adb_root_path_resolution_command(
                                &device_runtime,
                                &partition_name,
                            )
                            .map_err(nwflash_domain::DomainError::InvalidOperation)?;
                            let cancellation_for_resolution = cancellation.clone();
                            let resolved = task::spawn_blocking(move || {
                                run_command_with_cancel(resolution_command, None, move || {
                                    cancellation_for_resolution.is_cancelled()
                                })
                            })
                            .await
                            .map_err(|error| {
                                nwflash_domain::DomainError::Internal(format!(
                                    "ADB Root 分区校验调度失败：{error}"
                                ))
                            })??;
                            if resolved.exit_code != 0 {
                                return Err(nwflash_domain::DomainError::ExternalTool(format!(
                                    "ADB Root 分区校验失败，退出码 {}：{}",
                                    resolved.exit_code, resolved.stderr
                                )));
                            }
                            if resolved.stdout.trim() != expected_path {
                                return Err(nwflash_domain::DomainError::InvalidOperation(
                                    "分区设备路径已变化，请重新读取分区表后再执行。".to_string(),
                                ));
                            }
                        }

                        let mut command_result = Ok(());
                        for (command_index, command) in task_commands.commands.iter().enumerate() {
                            if cancellation.is_cancelled() {
                                command_result = Err(
                                    nwflash_domain::DomainError::UserCancelled(
                                        "运行被用户取消".to_string(),
                                    ),
                                );
                                break;
                            }

                            context.report_stage(format!(
                                "执行 {current}/{task_total}: {} ({}/{})",
                                command.program,
                                command_index + 1,
                                task_commands.commands.len()
                            ));
                            let output = run_process_command(
                                command_process(command),
                                Some(cancellation.clone()),
                            )
                            .await;
                            match output {
                                Ok(output) if output.exit_code == 0 => {}
                                Ok(output) => {
                                    command_result = Err(
                                        nwflash_domain::DomainError::ExternalTool(format!(
                                            "{} 执行失败，退出码 {}：{}",
                                            command.program, output.exit_code, output.stderr
                                        )),
                                    );
                                    break;
                                }
                                Err(error) => {
                                    command_result = Err(error);
                                    break;
                                }
                            }
                        }

                        if let Some(cleanup) = task_commands.cleanup_command.as_ref() {
                            let cleanup_result = run_process_command(command_process(cleanup), None)
                                .await
                                .and_then(|output| {
                                    if output.exit_code == 0 {
                                        Ok(())
                                    } else {
                                        Err(nwflash_domain::DomainError::ExternalTool(format!(
                                            "ADB Root 暂存文件清理失败，退出码 {}：{}",
                                            output.exit_code, output.stderr
                                        )))
                                    }
                                });
                            if let Err(cleanup_error) = cleanup_result {
                                command_result = match command_result {
                                    Ok(()) => Err(cleanup_error),
                                    Err(primary_error) => Err(
                                        nwflash_domain::DomainError::ExternalTool(format!(
                                            "{primary_error}；同时，{cleanup_error}"
                                        )),
                                    ),
                                };
                            }
                        }

                        command_result
                    }
                    .await;

                    match task_result {
                        Ok(()) => {
                            context
                                .report_partition_task(
                                    partition_name,
                                    nwflash_domain::PartitionTaskState::Succeeded,
                                    current as f64 / task_total as f64,
                                )
                                .await;
                        }
                        Err(error) => {
                            report_partition_terminal_updates(
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

                if let Some(slot) = switch_to_slot {
                    context.report_stage(format!("刷写完成，切换启动槽位 {slot}"));
                    let command = build_post_flash_slot_switch_for_current_device(
                        &device_runtime,
                        &slot,
                        &context,
                        cancellation.clone(),
                    )
                    .await?;
                    let cancellation_for_switch = cancellation.clone();
                    let output = task::spawn_blocking(move || {
                        run_command_with_cancel(command, None, move || {
                            cancellation_for_switch.is_cancelled()
                        })
                    })
                    .await
                    .map_err(|error| {
                        nwflash_domain::DomainError::Internal(format!(
                            "切换启动槽位调度失败：{error}"
                        ))
                    })??;
                    if output.exit_code != 0 {
                        return Err(nwflash_domain::DomainError::ExternalTool(format!(
                            "切换启动槽位失败，退出码 {}：{}",
                            output.exit_code, output.stderr
                        )));
                    }
                }

                if auto_reboot {
                    let reboot_command = build_post_flash_reboot_for_current_device(
                        &device_runtime,
                        &context,
                        cancellation.clone(),
                    )
                    .await?;
                    context.report_stage("刷写完成，正在重启设备");
                    let cancellation_for_reboot = cancellation.clone();
                    let output = task::spawn_blocking(move || {
                        run_command_with_cancel(reboot_command, None, move || {
                            cancellation_for_reboot.is_cancelled()
                        })
                    })
                    .await
                    .map_err(|error| {
                        nwflash_domain::DomainError::Internal(format!(
                            "设备重启调度失败：{error}"
                        ))
                    })??;
                    if output.exit_code != 0 {
                        return Err(nwflash_domain::DomainError::ExternalTool(format!(
                            "设备重启失败，退出码 {}：{}",
                            output.exit_code, output.stderr
                        )));
                    }
                }

                *execution_counts_for_run.lock().map_err(|_| {
                    nwflash_domain::DomainError::Internal(
                        "快速刷写执行计数锁不可用。".to_string(),
                    )
                })? = Some((
                    command_total + usize::from(has_slot_switch) + usize::from(auto_reboot),
                    command_total + usize::from(has_slot_switch) + usize::from(auto_reboot),
                ));
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let (command_count, executed_count) = execution_counts
        .lock()
        .map_err(|_| "快速刷写执行计数锁不可用。".to_string())?
        .take()
        .ok_or_else(|| "快速刷写执行未返回计数。".to_string())?;
    Ok(CommandExecutionResultDto {
        command_count,
        executed_count,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        },
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use futures::future::BoxFuture;
    use nwflash_application::{
        OperationAuthorization, OperationCoordinator, OperationCoordinatorError,
        OperationPermissionGate,
    };
    use nwflash_domain::{
        DomainError, PartitionExecutionPlan, PartitionOperationKind, PartitionTask,
        PartitionTransportKind,
    };
    use tokio::sync::Notify;

    struct BlockingQuickFlashAuthorization {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl OperationPermissionGate for BlockingQuickFlashAuthorization {
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

    struct BlockingQuickFlashDecision {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        authorization: OperationAuthorization,
    }

    impl OperationPermissionGate for BlockingQuickFlashDecision {
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

    fn write_plan() -> PartitionExecutionPlan {
        PartitionExecutionPlan {
            serial: "SN-001".to_string(),
            transport: PartitionTransportKind::Fastboot,
            operation: PartitionOperationKind::Write,
            tasks: vec![PartitionTask {
                partition_name: "boot".to_string(),
                device_path: "/dev/block/boot".to_string(),
                image_path: Some("C:\\tmp\\boot.img".to_string()),
                output_path: None,
                size_bytes: None,
            }],
        }
    }

    fn apply_fastboot_snapshot(runtime: &DeviceRuntime, serial: &str) {
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: serial.to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );
    }

    fn fastboot_probe_with_outputs(
        outputs: Vec<ProcessOutput>,
        calls: Arc<Mutex<Vec<ProcessCommand>>>,
    ) -> Arc<FastbootProbe> {
        let outputs = Arc::new(Mutex::new(VecDeque::from(outputs)));
        Arc::new(move |command, _cancellation| {
            calls
                .lock()
                .expect("probe call lock should not be poisoned")
                .push(command);
            let output = outputs
                .lock()
                .expect("probe output lock should not be poisoned")
                .pop_front()
                .expect("the test probe should have a queued output");
            Box::pin(async move { Ok(output) }) as FastbootProbeFuture
        })
    }

    fn device_discovery_with_snapshot(
        snapshot: nwflash_domain::DeviceSnapshot,
        called: Arc<AtomicBool>,
    ) -> Arc<DeviceDiscovery> {
        Arc::new(move || {
            called.store(true, Ordering::SeqCst);
            let snapshot = snapshot.clone();
            Box::pin(async move { Ok(snapshot) }) as DeviceDiscoveryFuture
        })
    }

    fn active_prepared_dual_slot_runtime() -> PreparedDualSlotRuntime {
        let scope = Arc::new(SessionCapabilityScope::new());
        scope.activate();
        PreparedDualSlotRuntime::with_scope(scope)
    }

    fn active_prepared_firmware_artifact_runtime() -> PreparedFirmwareArtifactRuntime {
        let scope = Arc::new(SessionCapabilityScope::new());
        scope.activate();
        PreparedFirmwareArtifactRuntime::with_scope(scope)
    }

    fn automatic_write_plan() -> PartitionExecutionPlan {
        let mut plan = write_plan();
        plan.transport = PartitionTransportKind::Automatic;
        plan
    }

    fn prepared_firmware_artifact_is_present(
        runtime: &PreparedFirmwareArtifactRuntime,
        artifact_id: &str,
    ) -> bool {
        runtime
            .prepared
            .lock()
            .expect("prepared firmware artifact lock should not be poisoned")
            .as_ref()
            .is_some_and(|entry| entry.artifact_id == artifact_id)
    }

    fn prepared_dual_slot_is_present(runtime: &PreparedDualSlotRuntime) -> bool {
        runtime
            .prepared
            .lock()
            .expect("prepared dual-slot plan lock should not be poisoned")
            .is_some()
    }

    #[test]
    fn quick_flash_prepare_commands_returns_dto_with_preview_commands() {
        let response =
            quick_flash_prepare_commands(write_plan()).expect("command dto should be prepared");

        assert_eq!(response.serial, "SN-001");
        assert_eq!(response.transport, "Fastboot");
        assert_eq!(response.operation, "Write");
        assert_eq!(response.task_count, 1);
        assert_eq!(response.commands.len(), 1);
        assert_eq!(
            response.commands[0].program,
            bundled_platform_tool("fastboot.exe")
        );
        assert!(response.commands[0].args.contains(&"flash".to_string()));
    }

    #[test]
    fn quick_flash_prepare_commands_rejects_bad_plan() {
        let mut plan = write_plan();
        plan.serial.clear();
        let result = quick_flash_prepare_commands(plan);

        let err = result.expect_err("serial empty should fail");
        assert!(err.contains("设备序列号不能为空"));
    }

    #[test]
    fn auto_reboot_option_builds_a_fastboot_reboot_command() {
        let commands = build_post_flash_commands("FAST-1", true);

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, bundled_platform_tool("fastboot.exe"));
        assert_eq!(commands[0].args, ["-s", "FAST-1", "reboot"]);
    }

    #[test]
    fn post_flash_slot_switch_builder_targets_the_given_serial() {
        let command = build_post_flash_slot_switch_command("FAST-1", "b");

        assert_eq!(command.program, bundled_platform_tool("fastboot.exe"));
        assert_eq!(command.args, ["-s", "FAST-1", "set_active", "b"]);
    }

    #[tokio::test]
    async fn post_flash_slot_switch_uses_fresh_fastbootd_discovery() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "SERIAL-A");
        let probe = fastboot_probe_with_outputs(
            vec![
                ProcessOutput {
                    exit_code: 0,
                    stdout: "SERIAL-B\tfastboot\n".to_string(),
                    stderr: String::new(),
                },
                ProcessOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: "is-userspace: yes\n".to_string(),
                },
            ],
            Arc::new(Mutex::new(Vec::new())),
        );

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test post-flash slot switch",
                move |context, cancellation| async move {
                    let command =
                        build_post_flash_slot_switch_for_current_device_with_fastboot_probe(
                            &runtime,
                            "b",
                            &context,
                            cancellation,
                            probe.as_ref(),
                        )
                        .await
                        .expect("the freshly discovered device should receive the slot switch");

                    assert_eq!(command.args, ["-s", "SERIAL-B", "set_active", "b"]);
                    Ok(())
                },
            )
            .await
            .expect("the post-flash test operation should complete");
    }

    #[tokio::test]
    async fn post_flash_reboot_uses_fresh_fastbootd_discovery() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "SERIAL-A");
        let probe = fastboot_probe_with_outputs(
            vec![
                ProcessOutput {
                    exit_code: 0,
                    stdout: "SERIAL-B\tfastboot\n".to_string(),
                    stderr: String::new(),
                },
                ProcessOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: "is-userspace: yes\n".to_string(),
                },
            ],
            Arc::new(Mutex::new(Vec::new())),
        );

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test post-flash reboot",
                move |context, cancellation| async move {
                    let command = build_post_flash_reboot_for_current_device_with_fastboot_probe(
                        &runtime,
                        &context,
                        cancellation,
                        probe.as_ref(),
                    )
                    .await
                    .expect("the freshly discovered device should receive the reboot");

                    assert_eq!(command.args, ["-s", "SERIAL-B", "reboot"]);
                    Ok(())
                },
            )
            .await
            .expect("the post-flash test operation should complete");
    }

    #[tokio::test]
    async fn post_flash_slot_switch_rejects_no_live_device_before_command_execution() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "SERIAL-A");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = fastboot_probe_with_outputs(
            vec![ProcessOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }],
            calls.clone(),
        );
        let post_action_built = Arc::new(AtomicBool::new(false));
        let command_executor_called = Arc::new(AtomicBool::new(false));
        let built_for_run = post_action_built.clone();
        let executed_for_run = command_executor_called.clone();

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test missing post-flash target",
                move |context, cancellation| async move {
                    let result =
                        build_post_flash_slot_switch_for_current_device_with_fastboot_probe(
                            &runtime,
                            "b",
                            &context,
                            cancellation,
                            probe.as_ref(),
                        )
                        .await
                        .inspect(|_command| {
                            built_for_run.store(true, Ordering::SeqCst);
                            executed_for_run.store(true, Ordering::SeqCst);
                        });

                    assert!(matches!(result, Err(DomainError::DeviceUnavailable(_))));
                    Ok(())
                },
            )
            .await
            .expect("the test operation should capture the missing post-action target");

        assert!(!post_action_built.load(Ordering::SeqCst));
        assert!(!command_executor_called.load(Ordering::SeqCst));
        let calls = calls
            .lock()
            .expect("probe call lock should not be poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, ["devices"]);
    }

    #[tokio::test]
    async fn post_flash_reboot_rejects_non_userspace_before_command_execution() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "SERIAL-A");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = fastboot_probe_with_outputs(
            vec![
                ProcessOutput {
                    exit_code: 0,
                    stdout: "SERIAL-B\tfastboot\n".to_string(),
                    stderr: String::new(),
                },
                ProcessOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: "(bootloader) is-userspace: no\n".to_string(),
                },
            ],
            calls.clone(),
        );
        let post_action_built = Arc::new(AtomicBool::new(false));
        let command_executor_called = Arc::new(AtomicBool::new(false));
        let built_for_run = post_action_built.clone();
        let executed_for_run = command_executor_called.clone();

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test non-userspace post-flash target",
                move |context, cancellation| async move {
                    let result = build_post_flash_reboot_for_current_device_with_fastboot_probe(
                        &runtime,
                        &context,
                        cancellation,
                        probe.as_ref(),
                    )
                    .await
                    .inspect(|_command| {
                        built_for_run.store(true, Ordering::SeqCst);
                        executed_for_run.store(true, Ordering::SeqCst);
                    });

                    assert!(matches!(result, Err(DomainError::DeviceUnavailable(_))));
                    Ok(())
                },
            )
            .await
            .expect("the test operation should capture the non-userspace rejection");

        assert!(!post_action_built.load(Ordering::SeqCst));
        assert!(!command_executor_called.load(Ordering::SeqCst));
        let calls = calls
            .lock()
            .expect("probe call lock should not be poisoned");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, ["devices"]);
        assert_eq!(
            calls[1].args,
            ["-s", "SERIAL-B", "getvar", "is-userspace"]
        );
    }

    #[tokio::test]
    async fn post_flash_slot_switch_rejects_multiple_live_fastboot_devices() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "SERIAL-A");
        let probe = fastboot_probe_with_outputs(
            vec![ProcessOutput {
                exit_code: 0,
                stdout: "SERIAL-A\tfastboot\nSERIAL-B\tfastboot\n".to_string(),
                stderr: String::new(),
            }],
            Arc::new(Mutex::new(Vec::new())),
        );

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test rejected post-flash slot switch",
                move |context, cancellation| async move {
                    let error =
                        build_post_flash_slot_switch_for_current_device_with_fastboot_probe(
                            &runtime,
                            "b",
                            &context,
                            cancellation,
                            probe.as_ref(),
                        )
                        .await
                        .expect_err("multiple live devices must reject before command construction");

                    assert!(matches!(error, DomainError::DeviceUnavailable(_)));
                    Ok(())
                },
            )
            .await
            .expect("the post-flash test operation should capture the rejection");
    }

    #[tokio::test]
    async fn post_flash_reboot_rejects_multiple_live_fastboot_devices() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "SERIAL-A");
        let probe = fastboot_probe_with_outputs(
            vec![ProcessOutput {
                exit_code: 0,
                stdout: "SERIAL-A\tfastboot\nSERIAL-B\tfastboot\n".to_string(),
                stderr: String::new(),
            }],
            Arc::new(Mutex::new(Vec::new())),
        );

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test rejected post-flash reboot",
                move |context, cancellation| async move {
                    let error = build_post_flash_reboot_for_current_device_with_fastboot_probe(
                        &runtime,
                        &context,
                        cancellation,
                        probe.as_ref(),
                    )
                    .await
                    .expect_err("multiple live devices must reject before command construction");

                    assert!(matches!(error, DomainError::DeviceUnavailable(_)));
                    Ok(())
                },
            )
            .await
            .expect("the post-flash test operation should capture the rejection");
    }

    #[test]
    fn fastboot_device_probe_ignores_non_fastboot_rows() {
        let serials = parse_fastboot_device_serials(
            "\nADB-1\tdevice\nFAST-1\tfastboot\nFAST-2 fastboot\nunauthorized\n",
        );

        assert_eq!(serials, ["FAST-1", "FAST-2"]);
    }

    #[test]
    fn fastboot_boolean_variable_requires_matching_label_and_exact_affirmative_value() {
        for (output, variable) in [
            ("(bootloader) is-userspace: yes", "is-userspace"),
            ("IS-USERSPACE: TRUE", "is-userspace"),
            ("(bootloader) HAS-SLOT:BOOT: 1", "has-slot:boot"),
            ("has-slot:init_boot: On", "has-slot:init_boot"),
        ] {
            assert!(
                is_true_fastboot_variable(output, variable),
                "{variable} should accept its exact affirmative value from {output:?}"
            );
        }

        for (output, variable) in [
            ("(bootloader) is-userspace: not true", "is-userspace"),
            ("(bootloader) is-userspace: true-ish", "is-userspace"),
            (
                "(bootloader) has-slot:vendor_boot: no\n(bootloader) unrelated: yes",
                "has-slot:vendor_boot",
            ),
            ("(bootloader) unrelated: yes", "is-userspace"),
            ("yes", "is-userspace"),
            ("(bootloader) is-userspace = yes", "is-userspace"),
        ] {
            assert!(
                !is_true_fastboot_variable(output, variable),
                "{variable} should reject a non-exact or unlabeled value from {output:?}"
            );
        }
    }

    #[test]
    fn waiting_for_fastbootd_rejects_multiple_connected_devices() {
        let error = select_waiting_fastboot_device(&["FAST-1".to_string(), "FAST-2".to_string()])
            .expect_err("waiting mode must not choose an arbitrary fastboot device");

        assert!(error.to_string().contains("多个 Fastboot 设备"));
    }

    #[test]
    fn adb_to_fastbootd_transition_accepts_the_sole_new_transport_serial() {
        let runtime = crate::commands::device::DeviceRuntime::new();
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::AdbConnected,
                serial: "SERIAL-A".to_string(),
                connection_label: "ADB 已连接".to_string(),
                model: "test".to_string(),
                android_version: "test".to_string(),
                battery_level: "test".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );
        let selected = select_waiting_fastboot_device(&["SERIAL-B".to_string()])
            .expect("one Fastboot device should be accepted")
            .expect("one Fastboot device should be selected");
        record_discovered_fastboot_device(&runtime, &selected);

        assert_eq!(selected, "SERIAL-B");
        assert_eq!(
            runtime
                .active_fastboot_serial()
                .expect("the discovered Fastboot target should replace the ADB snapshot"),
            "SERIAL-B"
        );
    }

    #[tokio::test]
    async fn execution_rejects_multiple_live_fastboot_devices_before_building_task_commands() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "FAST-A");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = fastboot_probe_with_outputs(
            vec![ProcessOutput {
                exit_code: 0,
                stdout: "FAST-A\tfastboot\nFAST-B\tfastboot\n".to_string(),
                stderr: String::new(),
            }],
            calls.clone(),
        );
        let task_commands_built = Arc::new(AtomicBool::new(false));
        let command_executor_called = Arc::new(AtomicBool::new(false));
        let built_for_run = task_commands_built.clone();
        let executed_for_run = command_executor_called.clone();
        let coordinator = OperationCoordinator::default();

        coordinator
            .run_async(
                OperationKind::Flashing,
                "test quick flash discovery",
                move |context, cancellation| async move {
                    let result = resolve_execution_plan_with_fastboot_probe(
                        &runtime,
                        write_plan(),
                        &context,
                        cancellation,
                        probe.as_ref(),
                    )
                    .await
                    .and_then(|plan| {
                        built_for_run.store(true, Ordering::SeqCst);
                        QuickFlashService::with_default_tools().build_task_commands(&plan)
                    })
                    .inspect(|_commands| {
                        executed_for_run.store(true, Ordering::SeqCst);
                    });

                    assert!(matches!(result, Err(DomainError::DeviceUnavailable(_))));
                    Ok(())
                },
            )
            .await
            .expect("the test operation should capture the discovery rejection");

        assert!(!task_commands_built.load(Ordering::SeqCst));
        assert!(!command_executor_called.load(Ordering::SeqCst));
        let calls = calls
            .lock()
            .expect("probe call lock should not be poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, ["devices"]);
    }

    #[tokio::test]
    async fn execution_rejects_non_exact_userspace_value_before_building_or_executing_commands() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "FAST-A");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = fastboot_probe_with_outputs(
            vec![
                ProcessOutput {
                    exit_code: 0,
                    stdout: "FAST-B\tfastboot\n".to_string(),
                    stderr: String::new(),
                },
                ProcessOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: "(bootloader) is-userspace: not true\n".to_string(),
                },
            ],
            calls.clone(),
        );
        let task_commands_built = Arc::new(AtomicBool::new(false));
        let command_executor_called = Arc::new(AtomicBool::new(false));
        let built_for_run = task_commands_built.clone();
        let executed_for_run = command_executor_called.clone();

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test non-userspace fastboot target",
                move |context, cancellation| async move {
                    let result = resolve_execution_plan_with_fastboot_probe(
                        &runtime,
                        write_plan(),
                        &context,
                        cancellation,
                        probe.as_ref(),
                    )
                    .await
                    .and_then(|plan| {
                        built_for_run.store(true, Ordering::SeqCst);
                        QuickFlashService::with_default_tools().build_task_commands(&plan)
                    })
                    .inspect(|_commands| {
                        executed_for_run.store(true, Ordering::SeqCst);
                    });

                    assert!(matches!(result, Err(DomainError::DeviceUnavailable(_))));
                    Ok(())
                },
            )
            .await
            .expect("the test operation should capture the non-userspace rejection");

        assert!(!task_commands_built.load(Ordering::SeqCst));
        assert!(!command_executor_called.load(Ordering::SeqCst));
        let calls = calls
            .lock()
            .expect("probe call lock should not be poisoned");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, ["devices"]);
        assert_eq!(
            calls[1].args,
            ["-s", "FAST-B", "getvar", "is-userspace"]
        );
    }

    #[tokio::test]
    async fn execution_rejects_no_live_fastboot_device_before_building_or_executing_commands() {
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "FAST-A");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = fastboot_probe_with_outputs(
            vec![ProcessOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            }],
            calls.clone(),
        );
        let task_commands_built = Arc::new(AtomicBool::new(false));
        let command_executor_called = Arc::new(AtomicBool::new(false));
        let built_for_run = task_commands_built.clone();
        let executed_for_run = command_executor_called.clone();

        OperationCoordinator::default()
            .run_async(
                OperationKind::Flashing,
                "test missing fastboot target",
                move |context, cancellation| async move {
                    let result = resolve_execution_plan_with_fastboot_probe(
                        &runtime,
                        write_plan(),
                        &context,
                        cancellation,
                        probe.as_ref(),
                    )
                    .await
                    .and_then(|plan| {
                        built_for_run.store(true, Ordering::SeqCst);
                        QuickFlashService::with_default_tools().build_task_commands(&plan)
                    })
                    .inspect(|_commands| {
                        executed_for_run.store(true, Ordering::SeqCst);
                    });

                    assert!(matches!(result, Err(DomainError::DeviceUnavailable(_))));
                    Ok(())
                },
            )
            .await
            .expect("the test operation should capture the missing-device rejection");

        assert!(!task_commands_built.load(Ordering::SeqCst));
        assert!(!command_executor_called.load(Ordering::SeqCst));
        let calls = calls
            .lock()
            .expect("probe call lock should not be poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, ["devices"]);
    }

    #[tokio::test]
    async fn execution_adb_root_fresh_discovery_rejects_non_adb_and_multiple_states() {
        let snapshots = [
            nwflash_domain::DeviceSnapshot::disconnected(),
            nwflash_domain::DeviceSnapshot::multiple_devices(),
        ];

        for snapshot in snapshots {
            let runtime = DeviceRuntime::new();
            runtime.apply_snapshot(
                nwflash_domain::DeviceSnapshot {
                    connection_state: nwflash_domain::DeviceConnectionState::AdbConnected,
                    serial: "ADB-CACHED".to_string(),
                    connection_label: "ADB 已连接".to_string(),
                    model: "cached".to_string(),
                    android_version: "cached".to_string(),
                    battery_level: "cached".to_string(),
                },
                false,
                nwflash_domain::DeviceRefreshMode::Manual,
            );
            let fastboot_calls = Arc::new(Mutex::new(Vec::new()));
            let fastboot_probe = fastboot_probe_with_outputs(Vec::new(), fastboot_calls.clone());
            let discovery_called = Arc::new(AtomicBool::new(false));
            let device_discovery =
                device_discovery_with_snapshot(snapshot, discovery_called.clone());
            let task_commands_built = Arc::new(AtomicBool::new(false));
            let command_executor_called = Arc::new(AtomicBool::new(false));
            let built_for_run = task_commands_built.clone();
            let executed_for_run = command_executor_called.clone();
            let mut plan = write_plan();
            plan.serial = "ADB-PREVIEW".to_string();
            plan.transport = PartitionTransportKind::AdbRoot;

            OperationCoordinator::default()
                .run_async(
                    OperationKind::Flashing,
                    "test invalid live adb target",
                    move |context, cancellation| async move {
                        let result = resolve_execution_plan_with_discovery(
                            &runtime,
                            plan,
                            &context,
                            cancellation,
                            fastboot_probe.as_ref(),
                            device_discovery.as_ref(),
                        )
                        .await
                        .and_then(|plan| {
                            built_for_run.store(true, Ordering::SeqCst);
                            QuickFlashService::with_default_tools().build_task_commands(&plan)
                        })
                        .inspect(|_commands| {
                            executed_for_run.store(true, Ordering::SeqCst);
                        });

                        assert!(matches!(result, Err(DomainError::DeviceUnavailable(_))));
                        Ok(())
                    },
                )
                .await
                .expect("the test operation should capture the invalid ADB target");

            assert!(discovery_called.load(Ordering::SeqCst));
            assert!(!task_commands_built.load(Ordering::SeqCst));
            assert!(!command_executor_called.load(Ordering::SeqCst));
            assert!(fastboot_calls
                .lock()
                .expect("probe call lock should not be poisoned")
                .is_empty());
        }
    }

    #[tokio::test]
    async fn execution_retargets_every_task_command_to_the_live_fastbootd_serial() {
        let runtime = crate::commands::device::DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "FAST-A");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let probe = fastboot_probe_with_outputs(
            vec![
                ProcessOutput {
                    exit_code: 0,
                    stdout: "FAST-B\tfastboot\n".to_string(),
                    stderr: String::new(),
                },
                ProcessOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: "(bootloader) is-userspace: yes\n".to_string(),
                },
            ],
            calls.clone(),
        );
        let mut plan = write_plan();
        plan.serial = "FAST-A".to_string();
        plan.tasks.push(PartitionTask {
            partition_name: "init_boot".to_string(),
            device_path: "/dev/block/init_boot".to_string(),
            image_path: Some("C:\\tmp\\init_boot.img".to_string()),
            output_path: None,
            size_bytes: None,
        });
        let coordinator = OperationCoordinator::default();

        coordinator
            .run_async(
                OperationKind::Flashing,
                "test quick flash retarget",
                move |context, cancellation| async move {
                    let plan = resolve_execution_plan_with_fastboot_probe(
                        &runtime,
                        plan,
                        &context,
                        cancellation,
                        probe.as_ref(),
                    )
                    .await
                    .expect("a sole live fastbootd device should retarget the plan");
                    let tasks = QuickFlashService::with_default_tools()
                        .build_task_commands(&plan)
                        .expect("the live-targeted plan should build flash commands");

                    assert_eq!(plan.serial, "FAST-B");
                    assert_eq!(tasks.len(), 2);
                    assert!(tasks.iter().all(|task| task.commands.iter().all(|command| {
                        command.args.first().map(String::as_str) == Some("-s")
                            && command.args.get(1).map(String::as_str) == Some("FAST-B")
                    })));
                    Ok(())
                },
            )
            .await
            .expect("the test operation should resolve and build commands");

        let calls = calls
            .lock()
            .expect("probe call lock should not be poisoned");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, ["devices"]);
        assert_eq!(
            calls[1].args,
            ["-s", "FAST-B", "getvar", "is-userspace"]
        );
    }

    #[test]
    fn prepared_plan_retargets_to_the_current_fastboot_snapshot() {
        let runtime = crate::commands::device::DeviceRuntime::new();
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "SERIAL-B".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let plan = QuickFlashService::with_default_tools()
            .retarget_execution_plan(&write_plan(), "SERIAL-B")
            .expect("a changed sole current serial should replace the prepared serial");
        let tasks = QuickFlashService::with_default_tools()
            .build_task_commands(&plan)
            .expect("retargeted plan should build flash commands");
        let slot_switch = build_post_flash_slot_switch_command(&plan.serial, "b");
        let reboot = build_post_flash_commands(&plan.serial, true)
            .into_iter()
            .next()
            .expect("auto reboot should build a command");

        assert_eq!(plan.serial, "SERIAL-B");
        assert_eq!(
            tasks[0].commands[0].args[0..4],
            ["-s", "SERIAL-B", "flash", "boot"]
        );
        assert_eq!(slot_switch.args, ["-s", "SERIAL-B", "set_active", "b"]);
        assert_eq!(reboot.args, ["-s", "SERIAL-B", "reboot"]);
    }

    #[tokio::test]
    async fn quick_flash_live_discovery_waits_for_authorization_and_rejects_multiple_devices() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingQuickFlashAuthorization {
                entered: entered.clone(),
                release: release.clone(),
            })),
            None,
            None,
            None,
        );
        let runtime = DeviceRuntime::new();
        apply_fastboot_snapshot(&runtime, "FAST-A");
        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls_during_authorization = calls.clone();
        let probe = fastboot_probe_with_outputs(
            vec![ProcessOutput {
                exit_code: 0,
                stdout: "FAST-A\tfastboot\nFAST-B\tfastboot\n".to_string(),
                stderr: String::new(),
            }],
            calls.clone(),
        );
        let command_executor_called = Arc::new(AtomicBool::new(false));
        let executed_for_run = command_executor_called.clone();
        let execution = coordinator.run_async(
            OperationKind::Flashing,
            "test authorized live discovery",
            move |context, cancellation| async move {
                let result = resolve_execution_plan_with_fastboot_probe(
                    &runtime,
                    write_plan(),
                    &context,
                    cancellation,
                    probe.as_ref(),
                )
                .await
                .inspect(|_plan| {
                    executed_for_run.store(true, Ordering::SeqCst);
                });

                assert!(matches!(result, Err(DomainError::DeviceUnavailable(_))));
                Ok(())
            },
        );
        let authorize = async {
            entered.notified().await;
            assert!(calls_during_authorization
                .lock()
                .expect("probe call lock should not be poisoned")
                .is_empty());
            release.notify_one();
        };
        let (result, ()) = tokio::join!(execution, authorize);

        result.expect("the deterministic test operation should capture the live rejection");
        assert!(!command_executor_called.load(Ordering::SeqCst));
        let calls = calls
            .lock()
            .expect("probe call lock should not be poisoned");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args, ["devices"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn firmware_artifact_validation_waits_for_flashing_admission() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingQuickFlashDecision {
                entered: entered.clone(),
                release: release.clone(),
                authorization: OperationAuthorization::allow(),
            })),
            None,
            None,
            None,
        );
        state.session_capabilities.activate();
        let missing_artifact_id = "firmware-missing-at-execution".to_string();
        state
            .prepared_firmware_artifact
            .replace(missing_artifact_id.clone(), automatic_write_plan())
            .expect("current session should publish the prepared firmware plan");

        let execution = quick_flash_execute_firmware_artifact_inner(
            &state,
            missing_artifact_id.clone(),
        );
        let observe_pending_authorization = async {
            entered.notified().await;
            assert!(prepared_firmware_artifact_is_present(
                &state.prepared_firmware_artifact,
                &missing_artifact_id
            ));
            assert!(matches!(
                state.operation_coordinator.try_acquire_idle(),
                Err(OperationCoordinatorError::InProgress)
            ));
            release.notify_one();
        };
        let (result, ()) = tokio::join!(execution, observe_pending_authorization);

        assert!(result.is_err());
        assert!(prepared_firmware_artifact_is_present(
            &state.prepared_firmware_artifact,
            &missing_artifact_id
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn denied_firmware_artifact_flash_preserves_capability_for_authorized_retry() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingQuickFlashDecision {
                entered: entered.clone(),
                release: release.clone(),
                authorization: OperationAuthorization::deny("test denial"),
            })),
            None,
            None,
            None,
        );
        state.session_capabilities.activate();
        let artifact_id = state.firmware_artifacts.replace(
            nwflash_domain::QuickFlashPartition::Boot,
            nwflash_domain::FlashImageInfo {
                path: r"C:\test-only\boot.img".to_string(),
                size_bytes: 4,
            },
            std::path::PathBuf::from(r"C:\test-only"),
        );
        state
            .prepared_firmware_artifact
            .replace(artifact_id.clone(), automatic_write_plan())
            .expect("current session should publish the prepared firmware plan");

        let execution =
            quick_flash_execute_firmware_artifact_inner(&state, artifact_id.clone());
        let observe_pending_authorization = async {
            entered.notified().await;
            assert!(prepared_firmware_artifact_is_present(
                &state.prepared_firmware_artifact,
                &artifact_id
            ));
            assert!(matches!(
                state.operation_coordinator.try_acquire_idle(),
                Err(OperationCoordinatorError::InProgress)
            ));
            release.notify_one();
        };
        let (denied, ()) = tokio::join!(execution, observe_pending_authorization);

        assert!(denied.is_err());
        assert!(prepared_firmware_artifact_is_present(
            &state.prepared_firmware_artifact,
            &artifact_id
        ));

        state.operation_coordinator = OperationCoordinator::default();
        let retry = quick_flash_execute_firmware_artifact_inner(&state, artifact_id.clone()).await;

        assert!(retry.is_err());
        assert!(!prepared_firmware_artifact_is_present(
            &state.prepared_firmware_artifact,
            &artifact_id
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn denied_dual_slot_flash_preserves_capability_for_authorized_retry() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingQuickFlashDecision {
                entered: entered.clone(),
                release: release.clone(),
                authorization: OperationAuthorization::deny("test denial"),
            })),
            None,
            None,
            None,
        );
        state.session_capabilities.activate();
        state
            .prepared_dual_slot
            .replace(PreparedPresetExecutionPlan {
                plan: automatic_write_plan(),
                switch_to_slot: Some("b".to_string()),
            })
            .expect("current session should publish the prepared dual-slot plan");

        let execution = quick_flash_execute_prepared_dual_slot_preset_inner(&state);
        let observe_pending_authorization = async {
            entered.notified().await;
            assert!(prepared_dual_slot_is_present(&state.prepared_dual_slot));
            assert!(matches!(
                state.operation_coordinator.try_acquire_idle(),
                Err(OperationCoordinatorError::InProgress)
            ));
            release.notify_one();
        };
        let (denied, ()) = tokio::join!(execution, observe_pending_authorization);

        assert!(denied.is_err());
        assert!(prepared_dual_slot_is_present(&state.prepared_dual_slot));

        state.operation_coordinator = OperationCoordinator::default();
        let retry = quick_flash_execute_prepared_dual_slot_preset_inner(&state).await;

        assert!(retry.is_err());
        assert!(!prepared_dual_slot_is_present(&state.prepared_dual_slot));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plan_provider_consumes_only_after_authorization_while_admission_remains_held() {
        let authorization_entered = Arc::new(Notify::new());
        let release_authorization = Arc::new(Notify::new());
        let mut state = AppState::new();
        state.operation_coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(BlockingQuickFlashDecision {
                entered: authorization_entered.clone(),
                release: release_authorization.clone(),
                authorization: OperationAuthorization::allow(),
            })),
            None,
            None,
            None,
        );
        state.session_capabilities.activate();
        state
            .prepared_dual_slot
            .replace(PreparedPresetExecutionPlan {
                plan: automatic_write_plan(),
                switch_to_slot: None,
            })
            .expect("current session should publish the prepared dual-slot plan");
        let state = Arc::new(state);
        let execution_state = state.clone();
        let prepared_dual_slot = state.prepared_dual_slot.clone();
        let (provider_consumed_tx, provider_consumed_rx) = mpsc::channel();
        let (release_provider_tx, release_provider_rx) = mpsc::channel();

        let execution = tokio::spawn(async move {
            quick_flash_execute_with_plan_provider(
                &execution_state,
                "test admitted provider".to_string(),
                move || {
                    let prepared = prepared_dual_slot.take()?;
                    provider_consumed_tx
                        .send(())
                        .expect("the test should observe provider consumption");
                    release_provider_rx
                        .recv()
                        .expect("the test should release the admitted provider");
                    Ok(QuickFlashExecutionRequest {
                        plan: prepared.plan,
                        auto_reboot: false,
                        switch_to_slot: prepared.switch_to_slot,
                    })
                },
            )
            .await
        });

        authorization_entered.notified().await;
        assert!(prepared_dual_slot_is_present(&state.prepared_dual_slot));
        assert!(matches!(
            state.operation_coordinator.try_acquire_idle(),
            Err(OperationCoordinatorError::InProgress)
        ));
        release_authorization.notify_one();
        tokio::task::spawn_blocking(move || {
            provider_consumed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("the provider should consume after authorization")
        })
        .await
        .expect("the provider observation task should join");

        assert!(!prepared_dual_slot_is_present(&state.prepared_dual_slot));
        assert!(matches!(
            state.operation_coordinator.try_acquire_idle(),
            Err(OperationCoordinatorError::InProgress)
        ));
        release_provider_tx
            .send(())
            .expect("the admitted provider should be released");

        assert!(execution
            .await
            .expect("the admitted execution task should join")
            .is_err());
    }

    #[test]
    fn batch_preset_plan_keeps_all_images_and_defers_slot_switch_until_after_flash() {
        let requests = [
            nwflash_domain::QuickFlashRequest {
                partition: QuickFlashPartition::Boot,
                image: FlashImageInfo {
                    path: "C:\\images\\boot.img".to_string(),
                    size_bytes: 8,
                },
            },
            nwflash_domain::QuickFlashRequest {
                partition: QuickFlashPartition::InitBoot,
                image: FlashImageInfo {
                    path: "C:\\images\\init_boot.img".to_string(),
                    size_bytes: 9,
                },
            },
        ];
        let options = nwflash_domain::QuickFlashOptions {
            target: nwflash_domain::FastbootTarget::Fastbootd,
            wait_for_device: true,
            flash_both_slots: true,
            switch_slot_after_flash: true,
            auto_reboot: true,
        };

        let prepared =
            build_batch_preset_execution_plan("FAST-1", &requests, &options, Some("a"), |_| true)
                .expect("both selected preset images should produce one execution plan");

        assert_eq!(prepared.plan.serial, "FAST-1");
        assert_eq!(
            prepared
                .plan
                .tasks
                .iter()
                .map(|task| task.partition_name.as_str())
                .collect::<Vec<_>>(),
            ["boot_a", "boot_b", "init_boot_a", "init_boot_b"]
        );
        assert_eq!(prepared.switch_to_slot.as_deref(), Some("b"));
    }

    #[test]
    fn inspect_image_path_projects_only_accepted_image_metadata() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-tauri-image-{nonce}.img"));
        fs::write(&path, [1, 2, 3]).expect("image fixture should be written");

        let response = inspect_image_path(path.to_string_lossy().as_ref())
            .expect("valid image should return its metadata");

        assert_eq!(response.path, path.to_string_lossy());
        assert_eq!(response.size_bytes, 3);
        fs::remove_file(path).expect("image fixture should be removed");
    }

    #[test]
    fn boot_plan_uses_the_current_fastboot_serial_without_accepting_a_frontend_serial() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-tauri-boot-{nonce}.img"));
        fs::write(&path, [1, 2, 3]).expect("image fixture should be written");
        let runtime = crate::commands::device::DeviceRuntime::new();
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "FAST-1".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let response = build_boot_image_plan(&runtime, path.to_string_lossy().as_ref())
            .expect("current fastboot device and checked image should build a boot plan");

        assert_eq!(response.serial, "FAST-1");
        assert_eq!(response.task_count, 1);
        assert_eq!(
            response.commands[0].args[0..4],
            ["-s", "FAST-1", "flash", "boot"]
        );
        fs::remove_file(path).expect("image fixture should be removed");
    }

    #[test]
    fn boot_execution_plan_revalidates_the_image_and_uses_the_current_fastboot_snapshot() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-tauri-execute-{nonce}.img"));
        fs::write(&path, [1, 2, 3]).expect("image fixture should be written");
        let runtime = crate::commands::device::DeviceRuntime::new();
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "FAST-2".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let plan = build_boot_execution_plan(&runtime, path.to_string_lossy().as_ref())
            .expect("checked image and Fastboot snapshot should create an execution plan");

        assert_eq!(plan.serial, "FAST-2");
        assert_eq!(plan.transport, PartitionTransportKind::Fastboot);
        assert_eq!(plan.tasks[0].partition_name, "boot");
        fs::remove_file(path).expect("image fixture should be removed");
    }

    #[test]
    fn dual_slot_preflight_reads_slot_capability_and_current_slot_from_the_active_fastboot_device()
    {
        let runtime = crate::commands::device::DeviceRuntime::new();
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "FAST-1".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let commands = build_dual_slot_preflight_commands(
            &runtime,
            nwflash_domain::QuickFlashPartition::InitBoot,
            true,
        )
        .expect("the active Fastboot snapshot should produce read-only slot checks");

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program, bundled_platform_tool("fastboot.exe"));
        assert_eq!(
            commands[0].args,
            ["-s", "FAST-1", "getvar", "has-slot:init_boot"]
        );
        assert_eq!(commands[1].args, ["-s", "FAST-1", "getvar", "current-slot"]);
    }

    #[test]
    fn verified_dual_slot_preflight_expands_both_slots_and_derives_the_next_slot() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-tauri-dual-slot-{nonce}.img"));
        fs::write(&path, [1, 2, 3]).expect("image fixture should be written");
        let runtime = crate::commands::device::DeviceRuntime::new();
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "FAST-4".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let prepared = build_verified_dual_slot_execution_plan(
            &runtime,
            path.to_string_lossy().as_ref(),
            nwflash_domain::QuickFlashPartition::Boot,
            "has-slot:boot: yes",
            Some("current-slot: a"),
            true,
        )
        .expect("verified A/B output should prepare both slots");

        assert_eq!(prepared.plan.serial, "FAST-4");
        assert_eq!(
            prepared
                .plan
                .tasks
                .iter()
                .map(|task| task.partition_name.as_str())
                .collect::<Vec<_>>(),
            ["boot_a", "boot_b"]
        );
        assert_eq!(prepared.switch_to_slot.as_deref(), Some("b"));
        fs::remove_file(path).expect("image fixture should be removed");
    }

    #[test]
    fn prepared_dual_slot_runtime_consumes_the_confirmed_plan_once() {
        let runtime = active_prepared_dual_slot_runtime();
        let prepared = PreparedPresetExecutionPlan {
            plan: PartitionExecutionPlan {
                serial: "FAST-5".to_string(),
                transport: PartitionTransportKind::Fastboot,
                operation: PartitionOperationKind::Write,
                tasks: vec![PartitionTask {
                    partition_name: "boot_a".to_string(),
                    device_path: "/dev/block/by-name/boot_a".to_string(),
                    image_path: Some(r"C:\images\boot.img".to_string()),
                    output_path: None,
                    size_bytes: Some(3),
                }],
            },
            switch_to_slot: Some("b".to_string()),
        };

        runtime
            .replace(prepared.clone())
            .expect("current session should publish the prepared plan");

        assert_eq!(
            runtime.take().expect("prepared plan should exist"),
            prepared
        );
        assert!(runtime.take().is_err());
    }

    #[test]
    fn prepared_dual_slot_plan_rejects_its_old_session_after_reactivation() {
        let state = AppState::new();
        state.session_capabilities.activate();
        state
            .prepared_dual_slot
            .replace(PreparedPresetExecutionPlan {
                plan: write_plan(),
                switch_to_slot: Some("b".to_string()),
            })
            .expect("current session should publish the prepared plan");
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should permit session revocation");

        state.revoke_root_capabilities(&idle_lease);
        state.session_capabilities.activate();

        assert!(state.prepared_dual_slot.take().is_err());
    }

    #[test]
    fn prepared_firmware_plan_rejects_its_old_session_after_reactivation() {
        let state = AppState::new();
        state.session_capabilities.activate();
        state
            .prepared_firmware_artifact
            .replace("artifact-old".to_string(), write_plan())
            .expect("current session should publish the prepared plan");
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should permit session revocation");

        state.revoke_root_capabilities(&idle_lease);
        state.session_capabilities.activate();

        assert!(state
            .prepared_firmware_artifact
            .take("artifact-old")
            .is_err());
    }

    #[test]
    fn old_firmware_artifact_confirmation_is_rejected_after_session_reactivation() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let external_root = std::env::temp_dir().join(format!("nwflash-old-artifact-{nonce}"));
        fs::create_dir_all(&external_root).expect("external fixture root should be created");
        let image_path = external_root.join("boot.img");
        fs::write(&image_path, [1, 2, 3]).expect("external fixture image should be written");

        let state = AppState::new();
        state.session_capabilities.activate();
        let artifact_id = state.firmware_artifacts.replace(
            QuickFlashPartition::Boot,
            FlashImageInfo {
                path: image_path.to_string_lossy().into_owned(),
                size_bytes: 3,
            },
            external_root.clone(),
        );
        let idle_lease = state
            .operation_coordinator
            .try_acquire_idle()
            .expect("idle state should permit session revocation");

        state.revoke_root_capabilities(&idle_lease);
        state.session_capabilities.activate();

        let error = prepare_firmware_artifact_confirmation(
            &state.firmware_artifacts,
            &state.device_runtime,
            &state.prepared_firmware_artifact,
            &artifact_id,
        )
        .expect_err("a prior-session artifact must be rejected");

        let prepared_runtime_error = state
            .prepared_firmware_artifact
            .take(&artifact_id)
            .expect_err("a rejected confirmation must not publish a prepared runtime entry");
        let external_root_preserved = external_root.is_dir();
        let _ = fs::remove_dir_all(&external_root);

        assert_eq!(error, "固件提取结果已失效，请重新提取。");
        assert_eq!(prepared_runtime_error, "请先完成固件刷写预检并确认刷写。");
        assert!(external_root_preserved);
    }

    #[test]
    fn partition_terminal_state_distinguishes_cancellation_from_failure() {
        assert_eq!(
            partition_terminal_state(&nwflash_domain::DomainError::UserCancelled(
                "停止".to_string()
            )),
            nwflash_domain::PartitionTaskState::Canceled
        );
        assert_eq!(
            partition_terminal_state(&nwflash_domain::DomainError::ExternalTool(
                "失败".to_string()
            )),
            nwflash_domain::PartitionTaskState::Failed
        );
    }

    #[test]
    fn partition_terminal_updates_mark_the_current_failure_and_skip_later_tasks() {
        let mut plan = write_plan();
        plan.tasks.push(PartitionTask {
            partition_name: "vendor_boot".to_string(),
            device_path: "/dev/block/vendor_boot".to_string(),
            image_path: Some("C:\\tmp\\vendor_boot.img".to_string()),
            output_path: None,
            size_bytes: None,
        });

        assert_eq!(
            partition_terminal_updates(
                &plan.tasks,
                0,
                &nwflash_domain::DomainError::ExternalTool("fastboot failure".to_string()),
            ),
            vec![
                nwflash_domain::PartitionTaskSnapshot {
                    partition_name: "boot".to_string(),
                    state: nwflash_domain::PartitionTaskState::Failed,
                    overall_progress: 0.0,
                },
                nwflash_domain::PartitionTaskSnapshot {
                    partition_name: "vendor_boot".to_string(),
                    state: nwflash_domain::PartitionTaskState::Canceled,
                    overall_progress: 0.0,
                },
            ]
        );
    }

    #[test]
    fn preset_execution_plan_accepts_only_a_known_partition_enum() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-tauri-init-boot-{nonce}.img"));
        fs::write(&path, [1, 2, 3]).expect("image fixture should be written");
        let runtime = crate::commands::device::DeviceRuntime::new();
        runtime.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "FAST-3".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let plan = build_preset_execution_plan(
            &runtime,
            path.to_string_lossy().as_ref(),
            nwflash_domain::QuickFlashPartition::InitBoot,
        )
        .expect("known preset should build an execution plan");

        assert_eq!(plan.tasks[0].partition_name, "init_boot");
        fs::remove_file(path).expect("image fixture should be removed");
    }

    #[test]
    fn firmware_artifact_plan_uses_the_runtime_image_without_accepting_a_browser_path() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let image_path = std::env::temp_dir().join(format!("nwflash-artifact-{nonce}.img"));
        fs::write(&image_path, [1, 2, 3]).expect("fixture image should be written");
        let artifacts = crate::commands::firmware::FirmwareArtifactRuntime::new();
        let artifact_id = artifacts.replace(
            QuickFlashPartition::Boot,
            FlashImageInfo {
                path: image_path.to_string_lossy().into_owned(),
                size_bytes: 3,
            },
            std::env::temp_dir(),
        );
        let device = crate::commands::device::DeviceRuntime::new();
        device.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "FAST-ARTIFACT".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let plan = build_firmware_artifact_execution_plan(&artifacts, &device, &artifact_id)
            .expect("runtime artifact should produce a preset plan");

        assert_eq!(plan.serial, "FAST-ARTIFACT");
        assert_eq!(plan.tasks[0].partition_name, "boot");
        assert_eq!(plan.tasks[0].image_path.as_deref(), image_path.to_str());
        fs::remove_file(image_path).expect("fixture image should be removed");
    }

    #[test]
    fn firmware_artifact_confirmation_exposes_only_the_partition_and_task_count() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let image_path = std::env::temp_dir().join(format!("nwflash-artifact-confirm-{nonce}.img"));
        fs::write(&image_path, [1, 2, 3]).expect("fixture image should be written");
        let artifacts = crate::commands::firmware::FirmwareArtifactRuntime::new();
        let artifact_id = artifacts.replace(
            QuickFlashPartition::Boot,
            FlashImageInfo {
                path: image_path.to_string_lossy().into_owned(),
                size_bytes: 3,
            },
            std::env::temp_dir(),
        );
        let device = crate::commands::device::DeviceRuntime::new();
        device.apply_snapshot(
            nwflash_domain::DeviceSnapshot {
                connection_state: nwflash_domain::DeviceConnectionState::FastbootConnected,
                serial: "FAST-CONFIRM".to_string(),
                connection_label: "Fastboot 已连接".to_string(),
                model: "--".to_string(),
                android_version: "--".to_string(),
                battery_level: "--".to_string(),
            },
            false,
            nwflash_domain::DeviceRefreshMode::Manual,
        );

        let prepared = active_prepared_firmware_artifact_runtime();
        let confirmation =
            prepare_firmware_artifact_confirmation(&artifacts, &device, &prepared, &artifact_id)
                .expect("the runtime artifact should produce a confirmation summary");

        assert_eq!(confirmation.partition, "boot");
        assert_eq!(confirmation.task_count, 1);
        fs::remove_file(image_path).expect("fixture image should be removed");
    }

    #[test]
    fn firmware_artifact_execution_requires_and_consumes_a_rust_only_preflight_capability() {
        let artifact_id = "firmware-one-shot".to_string();
        let plan = PartitionExecutionPlan {
            serial: "FAST-ONE-SHOT".to_string(),
            transport: PartitionTransportKind::Fastboot,
            operation: PartitionOperationKind::Write,
            tasks: vec![PartitionTask {
                partition_name: "boot".to_string(),
                device_path: "/dev/block/by-name/boot".to_string(),
                image_path: Some("C:\\internal\\boot.img".to_string()),
                output_path: None,
                size_bytes: Some(3),
            }],
        };
        let prepared = active_prepared_firmware_artifact_runtime();

        let direct = prepared
            .take(&artifact_id)
            .expect_err("execution without Rust preflight must be rejected");
        assert!(direct.contains("预检"));

        prepared
            .replace(artifact_id.clone(), plan.clone())
            .expect("current session should publish the prepared plan");
        assert_eq!(
            prepared
                .take(&artifact_id)
                .expect("preflighted artifact should execute once"),
            plan
        );

        let replay = prepared
            .take(&artifact_id)
            .expect_err("replaying a consumed artifact capability must be rejected");
        assert!(replay.contains("预检"));
    }

    #[test]
    fn adb_root_write_uses_a_device_staging_path_instead_of_process_standard_input() {
        let plan = PartitionExecutionPlan {
            serial: "ADB-1".to_string(),
            transport: PartitionTransportKind::AdbRoot,
            operation: PartitionOperationKind::Write,
            tasks: vec![PartitionTask {
                partition_name: "boot_a".to_string(),
                device_path: "/dev/block/sda12".to_string(),
                image_path: Some(r"C:\images\boot.img".to_string()),
                output_path: None,
                size_bytes: Some(64),
            }],
        };

        let tasks = QuickFlashService::with_default_tools()
            .build_task_commands(&plan)
            .expect("ADB Root write task should build");
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].staging_path.is_some());
        assert_eq!(tasks[0].commands.len(), 2);
        assert!(!tasks[0].commands[1]
            .args
            .iter()
            .any(|argument| argument.contains(r"C:\images\boot.img")));
    }

    #[test]
    fn adb_root_execution_revalidates_each_discovered_partition_path() {
        let plan = PartitionExecutionPlan {
            serial: "ADB-1".to_string(),
            transport: PartitionTransportKind::AdbRoot,
            operation: PartitionOperationKind::Erase,
            tasks: vec![PartitionTask {
                partition_name: "super".to_string(),
                device_path: "/dev/block/sda70".to_string(),
                image_path: None,
                output_path: None,
                size_bytes: None,
            }],
        };

        assert_eq!(
            adb_root_resolution_tasks(&plan),
            vec![Some(("super".to_string(), "/dev/block/sda70".to_string()))]
        );
    }
}
