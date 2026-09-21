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
    compute_targets, is_slot_based_mode, other_slot, should_simulate_partition_flash, DomainError,
    PartitionExecutionPlan, PartitionOperationKind, PartitionTask, PartitionTransportKind,
    SafeFlashSlotMode,
};
use nwflash_infrastructure::{
    build_download_target_path, download_to_file_with_cancellation, validate_available_space,
    OtaDiskSpaceProvider, OtaDownloadError, OtaDownloadProgressSink, SystemOtaDiskSpaceProvider,
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
    /// 「假刷写」字节数（判定见 [`should_simulate_partition_flash`]：
    /// 安全刷写下的受保护分区，以及保留 ROOT 勾选时的启动分区）。
    /// 为 `Some` 时该分区仍留在刷写队列里、日志照常显示刷入，但不会真正
    /// 写设备；执行阶段只按 `大小 / 35MB/s` 等待真实刷写所需的时间。
    ///
    /// 镜像**照常解包**（“假戏真做”：解包进度、耗时与临时占用与真机一致），
    /// 本字段取的就是落盘镜像的真实大小。
    pub simulated_flash_bytes: Option<u64>,
}

impl SafeFlashPartitionSource {
    /// 是否只在队列里做“假刷写”，绝不派发真实 fastboot 命令。
    pub fn is_simulated(&self) -> bool {
        self.simulated_flash_bytes.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct SafeFlashBuildOptions {
    pub serial: String,
    pub is_safe_flash: bool,
    pub is_keep_root: bool,
    /// 勾选「清除数据」：刷完分区后**不写 misc**，而是 `fastboot reboot recovery`
    /// 把设备重启到 REC，由用户手动执行清除数据。
    pub wipe_data: bool,
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

/// 用户对失败分区的处置决策（弹窗单选结果）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeFlashPartitionFailureDecision {
    /// 重试：使用同一受控命令再次刷写当前分区，不推进队列进度。
    Retry,
    /// 继续刷写：跳过当前失败分区的后续镜像，继续队列里其余命令
    /// （含其余分区、set_active、可选的清除数据与自动重启）。
    Continue,
    /// 中止：立刻停止剩余命令，操作按用户取消收尾。
    Abort,
}

/// 分区刷写失败时带回给上层的现场描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeFlashPartitionFailure {
    pub partition_name: String,
    pub error_message: String,
}

/// 线刷命令的墙钟超时（审计 A2：整条执行链此前完全无界，违反「任何
/// 命令不允许无限挂起」不变量——USB 半断开致 fastboot.exe 卡死时操作门
/// 永久占用、UI 永停「执行中」。C# `FastbootCliRunner` 对短命令 20/60s、
/// flash 用 IO 无进展 600s，没有任何 fastboot 命令无界）。
///
/// 这里只按命令的短/长二分接线：flash 走长兜底，其余（devices/getvar/
/// set_active/reboot）走短预算。兜底语义与 quick_flash 的 command_timeout
/// 分级（PROBE/GETVAR/CONTROL/FLASH）一致；本 crate 不依赖 tauri 层的
/// 分类器，避免反向依赖。
mod command_budget {
    use std::time::Duration;

    /// 短控制命令（devices/getvar/set_active/reboot）。
    pub const CONTROL: Duration = Duration::from_secs(60);
    /// 分区刷写长兜底：正常远小于此，防止半断开永久挂起。
    pub const FLASH: Duration = Duration::from_secs(30 * 60);

    /// 按命令参数形态挑选预算：`flash`/`dd` 走长兜底，其余短预算。
    /// 接收参数切片而非具体命令类型（CommandSpec/ProcessCommand 皆有 `.args`）。
    pub fn for_command(args: &[String]) -> Duration {
        let is_flash = args
            .iter()
            .any(|argument| matches!(argument.as_str(), "flash" | "dd"));
        if is_flash {
            FLASH
        } else {
            CONTROL
        }
    }
}

/// “假刷写”参数。只做假刷写的分区（见 [`should_simulate_partition_flash`]）
/// 不派发任何 fastboot 命令，但**必须**占用与真实刷写一致的墙钟时间，
/// 否则日志与耗时都会露馅。
mod simulated_flash {
    use std::time::Duration;

    /// 模拟速率：35 MB/s。取这个量级是因为它正是中低端 UFS 机型在
    /// fastbootd 下逐分区刷写的实测吞吐，`分区大小 ÷ 35MB/s` 得到的
    /// 等待时长与真机刷写基本重合。
    pub const SPEED_BYTES_PER_SECOND: u64 = 35 * 1024 * 1024;

    /// 等待切片粒度：每片之间检查一次取消，保证“停止操作”立刻生效，
    /// 而不会等整个分区模拟完。
    pub const SLICE: Duration = Duration::from_millis(50);

    /// `duration = 字节数 ÷ 35MB/s`。整型运算，避免浮点误差累积。
    pub fn duration(bytes: u64) -> Duration {
        Duration::from_millis(bytes.saturating_mul(1_000) / SPEED_BYTES_PER_SECOND)
    }
}

/// 刷写队列里的一步。分区刷写、`set_active`、重启到 REC 与收尾重启共用一条
/// 队列，靠标记区分语义。
#[derive(Debug, Clone)]
struct SafeFlashStep {
    command: ProcessCommand,
    /// 是否写入类命令（分区刷写）。
    is_flash: bool,
    /// 是否分区刷写命令（`fastboot flash <分区> <镜像>`）。
    is_partition_flash: bool,
    /// `Some(bytes)`：这一步只做假刷写，留在队列里按镜像大小模拟耗时，
    /// 不派发命令（判定见 [`should_simulate_partition_flash`]）。
    simulated_flash_bytes: Option<u64>,
    /// 失败是否只提示、不中止整轮（目前只用于 `reboot recovery`：机型不支持
    /// 该目标时设备仍停在 fastbootd，用户手动重启到 REC 即可完成清除数据）。
    tolerate_failure: bool,
}

impl SafeFlashStep {
    fn control(command: ProcessCommand) -> Self {
        Self {
            command,
            is_flash: false,
            is_partition_flash: false,
            simulated_flash_bytes: None,
            tolerate_failure: false,
        }
    }

    /// 失败只提示、不中止：用于收尾动作。
    fn tolerant_control(command: ProcessCommand) -> Self {
        Self {
            tolerate_failure: true,
            ..Self::control(command)
        }
    }
}

/// 勾选「清除数据」时，最后一步把设备重启到 REC，并在日志里给出手动清除步骤
/// （进 REC 后 adb/fastboot 都检测不到设备，只能由用户在 REC 界面里操作）。
pub const SAFE_FLASH_WIPE_DATA_MANUAL_STEPS: &str =
    "重启到REC（进REC后电脑就检测不到设备了）。请手动执行：清除数据-清除全部数据-确定-重启";

/// `reboot recovery` 失败时的提示：设备仍在 fastbootd，改为手动进 REC。
const SAFE_FLASH_WIPE_DATA_MANUAL_FALLBACK: &str =
    "未能自动重启到REC，请手动重启到 REC 后执行：清除数据-清除全部数据-确定-重启";

/// fastboot 协议错误（`FAILED (…)` / `remote error`）可能以退出码 0 伴随
/// stderr 输出出现。退出码为 0 时仍必须扫描输出，命中即判失败，防止
/// “半刷后继续刷下一分区”（审计 A6：quick_flash 通道早有此扫描，线刷
/// 完全信任退出码——同一 fastboot.exe、同一协议，两通道行为必须一致）。
fn fastboot_output_reports_failure(output: &ProcessOutput) -> Option<String> {
    let combined = format!("{}\n{}", output.stdout, output.stderr);
    let failure = combined
        .lines()
        .map(str::trim)
        .find(|line| {
            let line = line.strip_prefix("(bootloader)").unwrap_or(line).trim();
            (line.starts_with("FAILED")
                || line.starts_with("ERROR")
                || line.to_ascii_uppercase().contains("REMOTE ERROR"))
                && !line.is_empty()
        })
        .map(str::to_string)?;
    Some(failure)
}

#[derive(Clone)]
pub struct SafeFlashExecutionService {
    executor: Arc<dyn CancellableProcessExecutor>,
    /// 底层是否为真实系统执行器。逐命令留痕只对它有语义。
    system_executor: bool,
    tools: PlatformTools,
    fastbootd_attempts: usize,
    fastbootd_poll_interval: Duration,
}

impl SafeFlashExecutionService {
    /// 等待窗口 360 × 500ms = 180s，对齐 C# 全自动 ROOT 流程的
    /// `waitTimeout: TimeSpan.FromSeconds(180)`。
    const DEFAULT_FASTBOOTD_ATTEMPTS: usize = 360;

    pub fn new(executor: Arc<dyn CancellableProcessExecutor>) -> Self {
        Self {
            executor,
            system_executor: false,
            tools: PlatformTools::bundled(),
            fastbootd_attempts: Self::DEFAULT_FASTBOOTD_ATTEMPTS,
            fastbootd_poll_interval: Duration::from_millis(500),
        }
    }

    pub fn system() -> Self {
        Self {
            system_executor: true,
            ..Self::new(Arc::new(SystemCancellableProcessExecutor))
        }
    }

    /// 底层是不是真实系统执行器。
    ///
    /// 调用方注入的自定义执行器（测试用的假执行器）必须原样保留——只有真实
    /// 执行器才值得在它外面套逐命令留痕层，否则测试无法模拟设备输出。
    pub fn uses_system_executor(&self) -> bool {
        self.system_executor
    }

    /// 换掉底层进程执行器，保留本服务已有的工具路径与 fastbootd 等待参数。
    ///
    /// 生产路径用它在真实执行器外面套一层逐命令留痕
    /// （[`nwflash_windows::process::RecordingProcessExecutor`]），把每条
    /// adb/fastboot 命令的 argv / 退出码 / 输出上报服务器；执行语义不变。
    /// 换入的执行器由调用方负责，因此不再视作系统执行器。
    pub fn with_executor(mut self, executor: Arc<dyn CancellableProcessExecutor>) -> Self {
        self.executor = executor;
        self.system_executor = false;
        self
    }

    pub fn with_fastbootd_wait(mut self, attempts: usize, poll_interval: Duration) -> Self {
        self.fastbootd_attempts = attempts.max(1);
        self.fastbootd_poll_interval = poll_interval;
        self
    }

    pub fn execute<F, S, P>(
        &self,
        request: SafeFlashExecutionRequest<'_>,
        is_canceled: F,
        report_stage: S,
        report_progress: P,
    ) -> Result<SafeFlashExecutionResult, DomainError>
    where
        F: FnMut() -> bool,
        S: FnMut(String),
        P: FnMut(f64),
    {
        self.execute_with_partition_failure_hook(
            request,
            is_canceled,
            report_stage,
            report_progress,
            // 无回调即维持原语义:分区刷写失败直接中止整个执行。
            Option::<
                fn(
                    SafeFlashPartitionFailure,
                ) -> Result<SafeFlashPartitionFailureDecision, DomainError>,
            >::None,
        )
    }

    /// 与 [`Self::execute`] 相同的线刷执行，但分区刷写命令失败时会把
    /// 失败现场交给 `on_partition_failure` 决断：
    ///
    /// - 回调返回 [`SafeFlashPartitionFailureDecision::Continue`]：跳过该
    ///   失败分区的后续镜像，继续执行队列中的其余命令（其余分区、
    ///   set_active、可选的清除数据与自动重启）。
    /// - 返回 [`SafeFlashPartitionFailureDecision::Retry`]：使用相同的受控
    ///   命令重试当前分区，且不推进完成进度或计入成功/跳过数量。
    /// - 返回 [`SafeFlashPartitionFailureDecision::Abort`]：立刻停止剩余
    ///   命令，剩余流程按用户取消收尾，不追加任何恢复设备动作。
    /// - 回调为 `None`（未挂接）：保持原有行为，首个分区刷写失败立即
    ///   中止整个操作。
    ///
    /// 仅分区刷写（`fastboot flash <分区> <镜像>`）会触发回调；fastbootd
    /// 切换、getvar 预检、set_active、重启等控制命令失败仍按原语义处理。
    pub fn execute_with_partition_failure_hook<F, S, P, D>(
        &self,
        request: SafeFlashExecutionRequest<'_>,
        is_canceled: F,
        report_stage: S,
        report_progress: P,
        on_partition_failure: Option<D>,
    ) -> Result<SafeFlashExecutionResult, DomainError>
    where
        F: FnMut() -> bool,
        S: FnMut(String),
        P: FnMut(f64),
        D: FnMut(
            SafeFlashPartitionFailure,
        ) -> Result<SafeFlashPartitionFailureDecision, DomainError>,
    {
        // 无挂起闸门:既有调用点(含全部单测)行为完全不变。
        let mut never_suspended = || false;
        self.execute_with_suspend_gate(
            request,
            is_canceled,
            report_stage,
            report_progress,
            on_partition_failure,
            &mut never_suspended,
        )
    }

    /// 与 [`Self::execute_with_partition_failure_hook`] 相同，但额外接受一个
    /// **挂起查询**回调 `is_suspended`。
    ///
    /// 反调试语义(与"取消"严格区分):
    ///
    /// - `is_canceled()` 为真 -> 用户主动中止,按 `UserCancelled` 收尾。
    /// - `is_suspended()` 为真 -> **暂停推进**,返回 `DomainError::WriteSuspended`,
    ///   调用方据此挂起异步任务并提示用户;**绝不允许**在此路径上退出进程,
    ///   因为设备可能正处于写了一半的分区上。
    ///
    /// 挂起检查在每个命令**边界**执行:当前命令若已开始就让它跑完(中断一条
    /// 已发出的 fastboot 命令比等它结束更危险),只阻止下一条命令下发。
    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_suspend_gate<F, S, P, D, G>(
        &self,
        request: SafeFlashExecutionRequest<'_>,
        mut is_canceled: F,
        mut report_stage: S,
        mut report_progress: P,
        mut on_partition_failure: Option<D>,
        mut is_suspended: G,
    ) -> Result<SafeFlashExecutionResult, DomainError>
    where
        F: FnMut() -> bool,
        S: FnMut(String),
        P: FnMut(f64),
        D: FnMut(
            SafeFlashPartitionFailure,
        ) -> Result<SafeFlashPartitionFailureDecision, DomainError>,
        G: FnMut() -> bool,
    {
        let mut last_partition_target = String::new();
        let transport = DeviceTransport::new(self.tools.clone());
        let mut serial = request.serial.to_owned();
        let mut executed_command_count = 0usize;

        if request.transition_to_fastbootd {
            if is_network_adb_serial(&serial) {
                return Err(DomainError::InvalidOperation(
                    "网络 ADB 设备无法进入线刷所需模式，请使用 USB 连接后重试。".to_string(),
                ));
            }
            report_stage("正在重启设备".to_string());
            let command = transport
                .build_adb_reboot_fastboot_command(&serial)
                .map_err(|error| {
                    DomainError::InvalidOperation(format!("重启设备失败：{error}"))
                })?;
            self.run_required(command, &mut is_canceled, "重启设备")?;
            executed_command_count += 1;
        }
        report_stage("正在等待 fastbootd".to_string());
        serial = self.wait_for_fastbootd(&transport, request.serial, &mut is_canceled)?;

        let current_slot = if is_slot_based_mode(request.options.slot_mode) {
            // getvar 瞬态抖动时安全降级：读不到 current-slot 就当非 A/B
            // 设备处理，目标回退分区原名、不追加 set_active（C# 参考
            // SafeFlashSlotPlanner 的“回退原样刷写不砖机”语义），而不是
            // 直接放弃整次刷写。
            let current_slot = self
                .read_fastboot_var(&transport, &serial, "current-slot", &mut is_canceled)
                .unwrap_or_default();
            normalize_slot_name(&current_slot)
        } else {
            None
        };

        let mut flash_steps = Vec::new();
        let mut skipped_partition_count = 0usize;
        for source in &request.source.partitions {
            self.ensure_not_canceled(&mut is_canceled)?;

            let has_slot = if is_slot_based_mode(request.options.slot_mode) {
                let variable = format!("has-slot:{}", source.partition_name);
                // has-slot 读不到按 false 处理（C# HasSlotAsync 查询失败按
                // false 回退原样刷写），瞬态 getvar 失败不放弃整次刷写。
                let has_slot = self
                    .read_fastboot_var(&transport, &serial, &variable, &mut is_canceled)
                    .unwrap_or_default();
                parse_slot_flag(&has_slot).unwrap_or(false)
            } else {
                source.has_slot
            };
            for target in compute_targets(
                &source.partition_name,
                request.options.slot_mode,
                current_slot.as_deref(),
                has_slot,
            ) {
                // 受保护分区（安全刷写）与保留 ROOT 的启动分区照旧进队列，
                // 只是绝不会真的写设备：刷写日志、分区序号与耗时都与真实
                // 刷写完全一致，判定在域层统一维护。
                let simulated_flash_bytes = should_simulate_partition_flash(
                    &target,
                    request.options.is_safe_flash,
                    request.options.is_keep_root,
                )
                .then(|| source.simulated_flash_bytes.unwrap_or(0));
                flash_steps.push(SafeFlashStep {
                    command: transport
                        .build_fastboot_flash_command(&serial, &target, &source.image_path)
                        .map_err(|error| {
                            DomainError::InvalidOperation(format!("生成刷写命令失败：{error}"))
                        })?,
                    is_flash: true,
                    is_partition_flash: true,
                    simulated_flash_bytes,
                    tolerate_failure: false,
                });
            }
        }

        if flash_steps.is_empty() {
            return Err(DomainError::InvalidOperation(
                "未发现可刷写分区（可能设备分区与固件不匹配）。".to_string(),
            ));
        }

        let mut commands = flash_steps;
        if request.options.slot_mode == SafeFlashSlotMode::OtherSlot {
            // current-slot 可读即追加 set_active：has_slot=false 只把刷写
            // 目标降级为分区原名，不取消槽位切换（C# 语义，配套测试
            // execution_degrades_to_partition_original_name_when_has_slot_is_unreadable）。
            if let Some(next_slot) = other_slot(current_slot.as_deref()) {
                commands.push(SafeFlashStep::control(
                    transport
                        .build_fastboot_set_active_command(&serial, next_slot)
                        .map_err(|error| {
                            DomainError::InvalidOperation(format!("切换槽位失败：{error}"))
                        })?,
                ));
            }
        }
        if request.options.wipe_data {
            // 「清除数据」不再往 misc 写 BCB：改为把设备重启到 REC，由用户在
            // REC 界面里手动清除数据（进 REC 后 adb/fastboot 都检测不到设备，
            // 程序无法代劳）。因此这一步就是队列的最后一步——设备离开
            // fastboot 后不该再派发任何 fastboot 命令。
            //
            // 失败只提示不中止：机型不支持 `reboot recovery` 时设备仍停在
            // fastbootd，用户手动重启到 REC 同样能完成清除数据，没有理由把
            // 已经刷好的整轮判失败。
            commands.push(SafeFlashStep::tolerant_control(
                transport
                    .build_fastboot_reboot_target_command(&serial, Some("recovery"))
                    .map_err(|error| {
                        DomainError::InvalidOperation(format!("生成重启到REC命令失败：{error}"))
                    })?,
            ));
        } else {
            commands.push(SafeFlashStep::control(
                transport.build_fastboot_reboot_command(&serial).map_err(|error| {
                    DomainError::InvalidOperation(format!("重启设备失败：{error}"))
                })?,
            ));
        }

        let command_total = commands.len();
        let command_count = command_total + usize::from(request.transition_to_fastbootd);
        let mut flashed_partition_count = 0usize;
        let mut remaining = commands;
        let mut index = 0usize;
        let partition_total = remaining
            .iter()
            .filter(|step| step.is_partition_flash)
            .count();
        let mut partition_index = 0usize;
        report_stage("正在刷写".to_string());
        while !remaining.is_empty() {
            let step = remaining.remove(0);
            let SafeFlashStep {
                command,
                is_flash,
                is_partition_flash,
                simulated_flash_bytes,
                tolerate_failure,
            } = step.clone();
            index += 1;
            self.ensure_not_canceled(&mut is_canceled)?;
            // 反调试挂起检查(**不是取消**):命中即停止推进,但绝不退出进程、
            // 也绝不关闭 fastboot 会话——设备可能正处于写了一半的分区上,
            // 中断才是真正的变砖风险。已有命令跑完再停,避免打断半条写入。
            if is_suspended() {
                return Err(DomainError::WriteSuspended(
                    "检测到调试器，写入已暂停以确保设备安全。请处理调试工具后继续。".to_string(),
                ));
            }
            if is_partition_flash {
                // fastboot flash 参数形态固定为 [-s, serial, flash, 分区, 镜像]
                last_partition_target = command
                    .args
                    .iter()
                    .position(|argument| argument == "flash")
                    .and_then(|position| command.args.get(position + 1))
                    .cloned()
                    .unwrap_or_default();
                partition_index += 1;
                report_stage(format!(
                    "刷写分区[{partition_index}/{partition_total}] ..."
                ));
            } else if tolerate_failure {
                // 重启到 REC：先把「进 REC 后要手动做什么」写给用户，
                // 设备进入 REC 后程序就再也探测不到它了。
                report_stage(SAFE_FLASH_WIPE_DATA_MANUAL_STEPS.to_string());
            }
            report_progress(index as f64 / command_total as f64);
            let run_result = if let Some(bytes) = simulated_flash_bytes {
                // 受保护分区：只等时间，不发命令，日志与真实刷写一字不差。
                self.run_simulated_flash(bytes, &mut is_canceled)
            } else if is_partition_flash {
                self.run_partition_flash(command, &mut is_canceled)
            } else {
                self.run_required(command, &mut is_canceled, "fastboot 命令")
            };
            match run_result {
                Ok(_) => {
                    executed_command_count += 1;
                    if is_flash {
                        flashed_partition_count += 1;
                        if is_partition_flash {
                            report_stage(format!(
                                "刷写分区[{partition_index}/{partition_total}] ... OK"
                            ));
                        }
                    }
                }
                Err(error) => {
                    // 主动停止/会话取消不是分区刷写失败：直接透传取消，
                    // 避免在取消收尾期间误触发前端失败决策弹窗。
                    if matches!(error, DomainError::UserCancelled(_)) {
                        return Err(error);
                    }
                    if is_partition_flash {
                        report_stage(format!(
                            "刷写分区[{partition_index}/{partition_total}] ... 失败"
                        ));
                    }
                    if tolerate_failure {
                        // 收尾动作失败不算整轮失败：设备只是没自动进 REC，
                        // 用户手动重启到 REC 同样能清除数据，日志给出替代步骤。
                        report_stage(SAFE_FLASH_WIPE_DATA_MANUAL_FALLBACK.to_string());
                        continue;
                    }
                    if !is_partition_flash || on_partition_failure.is_none() {
                        // 无决策回调时保持原语义：分区刷写失败以脱敏的
                        // 统一文案上报，绝不能把 fastboot 原始输出（可能
                        // 含下载地址/令牌）拼进对外错误。
                        if is_partition_flash {
                            return Err(DomainError::ExternalTool(
                                "fastboot 命令执行失败。".to_string(),
                            ));
                        }
                        return Err(error);
                    }
                    // 仅分区刷写失败交给用户决策；决策勾选的恢复动作
                    // 追加到队列末尾，由同一循环按顺序执行。
                    let decision = on_partition_failure.as_mut().expect("已确认回调存在")(
                        SafeFlashPartitionFailure {
                            partition_name: last_partition_target.clone(),
                            error_message: error.to_string(),
                        },
                    )?;
                    match decision {
                        SafeFlashPartitionFailureDecision::Retry => {
                            // 重试保持在同一个队列位置；失败尝试既不是完成
                            // 步骤，也不是跳过分区，因此回退索引并把原受控
                            // 命令放回队首。下一次进度仍为相同的 i / total。
                            index -= 1;
                            partition_index -= 1;
                            remaining.insert(0, step);
                            continue;
                        }
                        SafeFlashPartitionFailureDecision::Continue => {
                            skipped_partition_count += 1;
                            continue;
                        }
                        SafeFlashPartitionFailureDecision::Abort => {
                            return Err(DomainError::UserCancelled(
                                "用户在分区刷写失败后选择中止".to_string(),
                            ));
                        }
                    }
                }
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
        let budget = command_budget::for_command(&command.args);
        let output = self
            .executor
            .run_with_timeout(command, Some(budget), is_canceled)
            .map_err(|error| match error {
                DomainError::UserCancelled(_) => {
                    DomainError::UserCancelled("运行被用户取消".to_string())
                }
                _ => DomainError::ExternalTool(format!("{label}执行失败。")),
            })?;
        if output.exit_code == 0 {
            if let Some(failure) = fastboot_output_reports_failure(&output) {
                return Err(DomainError::ExternalTool(format!(
                    "{label}报告协议失败：{failure}"
                )));
            }
            Ok(output)
        } else {
            Err(DomainError::ExternalTool(format!(
                "{label}执行失败，退出码 {}。",
                output.exit_code
            )))
        }
    }

    /// 分区刷写命令的执行：失败时把 fastboot 的原始输出（stdout/stderr
    /// 合并）带回给调用方，供“分区刷写失败”弹窗展示具体报错日志；
    /// 成功语义与 [`Self::run_required`] 完全一致。
    fn run_partition_flash<F>(
        &self,
        command: ProcessCommand,
        is_canceled: &mut F,
    ) -> Result<ProcessOutput, DomainError>
    where
        F: FnMut() -> bool,
    {
        self.ensure_not_canceled(is_canceled)?;
        let output = self
            .executor
            .run_with_timeout(command, Some(command_budget::FLASH), is_canceled)
            .map_err(|error| match error {
                DomainError::UserCancelled(_) => {
                    DomainError::UserCancelled("运行被用户取消".to_string())
                }
                _ => DomainError::ExternalTool("fastboot 命令执行失败。".to_string()),
            })?;
        if output.exit_code == 0 {
            // 退出码 0 不等于成功：fastboot 协议失败（FAILED (remote:…)）
            // 常以退出码 0 出现。不扫描就继续刷下一分区＝半刷状态机。
            if let Some(failure) = fastboot_output_reports_failure(&output) {
                return Err(DomainError::ExternalTool(fastboot_failure_summary_with(
                    &output,
                    &failure,
                )));
            }
            Ok(output)
        } else {
            Err(DomainError::ExternalTool(fastboot_failure_summary(&output)))
        }
    }

    /// 等待计划设备进入 fastbootd（对齐 C# `MatchesTargetAsync` 的
    /// `expectedSerial` 逐次比对语义）：发现的设备不是计划设备时视为
    /// “目标还没现身”，继续等待——绝不能把 A 机修补的镜像刷进碰巧
    /// 在 fastboot 模式的 B 机。窗口 180s（C# 全自动 ROOT waitTimeout），
    /// 超时明确报错并释放操作门。
    ///
    /// 单次 `fastboot devices` 抖动或传输失败不炸掉整个等待（审计 A7：
    /// 同仓 quick_flash 的等价循环把探测失败按「继续等」处理，两处语义
    /// 必须一致）——记录后本轮作废继续等。窗口按墙钟截止（而非仅按
    /// 尝试次数）：探测自身超时（60s 档）时 360 次重试会远超 180s，
    /// 必须以时间为准收口。
    fn wait_for_fastbootd<F>(
        &self,
        transport: &DeviceTransport,
        expected_serial: &str,
        is_canceled: &mut F,
    ) -> Result<String, DomainError>
    where
        F: FnMut() -> bool,
    {
        let window = self.fastbootd_window();
        let deadline = std::time::Instant::now() + window;
        for attempt in 0..self.fastbootd_attempts {
            self.ensure_not_canceled(is_canceled)?;
            // 墙钟截止从第二次尝试起生效：至少完成一次探测，保证测试的
            // attempts=1/interval=0 配置（以及生产中极短窗口的边界）行为
            // 与旧语义一致。
            if attempt > 0 && std::time::Instant::now() >= deadline {
                break;
            }
            let probe = self
                .tools
                .fastboot_devices_command()
                .map_err(|error| {
                    DomainError::InvalidOperation(format!("检测 fastbootd 失败：{error}"))
                });
            let output = match probe
                .and_then(|command| self.run_required(command, is_canceled, "检测 fastbootd"))
            {
                Ok(output) => output,
                Err(DomainError::UserCancelled(_)) => return Err(DomainError::UserCancelled(
                    "运行被用户取消".to_string(),
                )),
                // 瞬态探测失败：本轮作废，继续等待窗口。
                Err(_) => {
                    if attempt + 1 < self.fastbootd_attempts {
                        thread::sleep(self.fastbootd_poll_interval);
                    }
                    continue;
                }
            };
            if let Some(serial) = sole_fastboot_device_serial(&output.stdout)? {
                if serial != expected_serial {
                    // 计划设备还没现身（或在线的是另一台）：继续等待，
                    // 绝不切换执行目标。
                    if attempt + 1 < self.fastbootd_attempts {
                        thread::sleep(self.fastbootd_poll_interval);
                    }
                    continue;
                }
                let userspace =
                    match self.read_fastboot_var(transport, &serial, "is-userspace", is_canceled) {
                        Ok(userspace) => userspace,
                        Err(DomainError::UserCancelled(_)) => {
                            return Err(DomainError::UserCancelled(
                                "运行被用户取消".to_string(),
                            ));
                        }
                        // getvar 瞬态失败按「还没进入 fastbootd」继续等。
                        Err(_) => {
                            if attempt + 1 < self.fastbootd_attempts {
                                thread::sleep(self.fastbootd_poll_interval);
                            }
                            continue;
                        }
                    };
                if is_affirmative_flag(&userspace) {
                    return Ok(serial);
                }
            }
            if attempt + 1 < self.fastbootd_attempts {
                thread::sleep(self.fastbootd_poll_interval);
            }
        }
        Err(DomainError::DeviceUnavailable(
            format!(
                "等待预检设备 {expected_serial} 进入 fastbootd 超时（{} 秒），请确认设备已重新连接后重试。",
                self.fastbootd_wait_seconds()
            ),
        ))
    }

    /// fastbootd 等待窗口总时长：attempts × poll_interval。
    fn fastbootd_window(&self) -> Duration {
        let total_millis =
            (self.fastbootd_attempts as u64).saturating_mul(self.fastbootd_poll_interval.as_millis() as u64);
        Duration::from_millis(total_millis)
    }

    /// fastbootd 等待窗口（秒）：attempts × poll_interval 换算。
    fn fastbootd_wait_seconds(&self) -> u64 {
        // 实际窗口 = attempts × interval（360 × 500ms = 180s）。旧实现
        // `as_secs().max(1)` 把 500ms 算成 1s，文案夸大一倍（审计 A17）。
        self.fastbootd_window().as_secs().max(1)
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

    /// 只做假刷写的分区的“假刷写”：不派发任何命令，只按镜像大小 ÷ 35MB/s
    /// 等待，让阶段日志与总耗时和真实刷入一致。
    ///
    /// 等待分片进行，每片之间检查取消——用户点“停止操作”必须立刻收尾，
    /// 而不是等整个分区模拟完（一个 system.img 的模拟时长可达分钟级）。
    fn run_simulated_flash<F>(
        &self,
        bytes: u64,
        is_canceled: &mut F,
    ) -> Result<ProcessOutput, DomainError>
    where
        F: FnMut() -> bool,
    {
        self.ensure_not_canceled(is_canceled)?;
        let mut remaining = simulated_flash::duration(bytes);
        while !remaining.is_zero() {
            let slice = remaining.min(simulated_flash::SLICE);
            thread::sleep(slice);
            remaining -= slice;
            self.ensure_not_canceled(is_canceled)?;
        }
        // 与真实成功刷写同样返回退出码 0：调用方按“刷写成功”计入日志与
        // 计数，对外看不出这条分区没有真的写设备。
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
        })
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

/// 汇总一次失败的 fastboot 进程输出，供分区失败决策回调（前端弹窗）
/// 展示。原始 stdout/stderr 先经 trace 脱敏管线（凭据/私钥类内容拒绝
/// 或替换，与崩溃补传同一管线），再截断到安全长度避免撑爆弹窗。
fn fastboot_failure_summary(output: &ProcessOutput) -> String {
    fastboot_failure_summary_with(output, &format!("退出码 {}。", output.exit_code))
}

/// [`fastboot_failure_summary`] 的变体：退出码 0 但扫描命中协议失败行时，
/// 以协议失败行开头（退出码为 0 的信息只会误导排障）。
fn fastboot_failure_summary_with(output: &ProcessOutput, headline: &str) -> String {
    const MAX_LOG_BYTES: usize = 2000;
    let mut combined = headline.to_string();
    for stream in [&output.stdout, &output.stderr] {
        let stream = stream.trim();
        if stream.is_empty() {
            continue;
        }
        if !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(stream);
        combined.push('\n');
    }
    let combined = redact_fastboot_failure_text(&combined);
    if combined.len() > MAX_LOG_BYTES {
        // 截断到字节上限附近的 UTF-8 字符边界。
        let mut end = MAX_LOG_BYTES;
        while !combined.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n…（日志已截断）", &combined[..end])
    } else {
        combined
    }
}

/// trace 脱敏管线（与 crash_uploader/trace-v2 上传一致）：私钥等高危
/// 内容整段拒绝，凭据类替换为占位；序列号/路径有诊断价值原样保留。
/// 整段被拒绝时只保留退出码摘要，不把原文带给弹窗。
fn redact_fastboot_failure_text(text: &str) -> String {
    use std::io::Cursor;

    use nwflash_domain::{TraceId, TraceOutputStreamV2};
    use nwflash_protection::{ExactSecretSet, TraceOutputSession};

    if text.is_empty() {
        return String::new();
    }
    let Ok(event_id) = TraceId::try_new_v7() else {
        return String::new();
    };
    let secrets = ExactSecretSet::empty();
    let mut reader = Cursor::new(text.as_bytes());
    let Ok(session) = TraceOutputSession::from_reader(
        event_id,
        TraceOutputStreamV2::Stdout,
        &mut reader,
        &secrets,
    ) else {
        return "（失败日志包含敏感内容，已隐藏。）".to_string();
    };
    let Ok(uploads) = session.into_upload_attempts() else {
        return "（失败日志包含敏感内容，已隐藏。）".to_string();
    };
    let mut redacted = String::with_capacity(text.len());
    for upload in &uploads {
        for chunk in upload.output_chunks() {
            redacted.push_str(chunk.text());
        }
    }
    redacted
}

#[derive(Debug, Clone)]
pub struct SafeFlashService;

impl Default for SafeFlashService {
    fn default() -> Self {
        Self
    }
}

impl SafeFlashService {
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

            // 计划里保留全部条目：受保护分区与保留 ROOT 的启动分区同样会
            // 出现在刷写队列与日志里（假刷写），预检计数必须与之一致。
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

        // 「清除数据」不再产生刷写任务：它现在是收尾的 `fastboot reboot
        // recovery`，不是对 misc 的写入，因此预检计数只数固件分区。
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
            // 「假戏真做」：只做假刷写的分区（受保护分区、保留 ROOT 的启动
            // 分区）同样要真的解包——解包进度、耗时与临时占用必须与真机刷写
            // 一致，唯一被模拟的是「写进设备」那一步。
            let selected = inspection.entries;
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
                .map(|(entry, image)| {
                    // 模拟耗时直接取解包出来的真实镜像大小。
                    let simulated_flash_bytes = should_simulate_partition_flash(
                        &entry.name,
                        options.is_safe_flash,
                        options.is_keep_root,
                    )
                    .then(|| u64::try_from(image.size_bytes).unwrap_or(0));
                    SafeFlashPartitionSource {
                        partition_name: entry.name,
                        image_path: image.path,
                        has_slot: true,
                        simulated_flash_bytes,
                    }
                })
                .collect();
            Ok(SafeFlashPreparedSource {
                staging_root: Some(staging_root.clone()),
                partitions,
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
            // 解包目录来源直接读用户盘上的镜像，不需要临时目录。
            let partitions = self
                .list_directory_images(path, options, cancellation)
                .map_err(map_preparation_io_error)?;
            return Ok(SafeFlashPreparedSource {
                staging_root: None,
                partitions,
                has_block_based_content: false,
            });
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
        let staging_root = (extension == "zip").then(|| self.create_staging_root());
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
                self.list_single_image(path, options)?
            } else {
                return Err(DomainError::InvalidFormat(
                    "仅支持 .zip/.img/.bin 来源。".to_string(),
                ));
            };

            self.ensure_preparation_not_canceled(cancellation)?;

            Ok(SafeFlashPreparedSource {
                staging_root: staging_root.clone(),
                partitions,
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
                "在线固件地址不能为空。".to_string(),
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

            // 与固件包一起下载它的 detached 签名。签名必须来自**同一个**
            // 渠道，否则本地验签就退化成"用攻击者提供的公钥验证攻击者的包"。
            download_firmware_signature(url, &download_target, cancellation).await?;
            self.ensure_preparation_not_canceled(cancellation)?;

            let mut archive = ZipArchive::new(File::open(&download_target).map_err(|error| {
                DomainError::InvalidOperation(format!("打开固件压缩包失败：{error}"))
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


            Ok(SafeFlashPreparedSource {
                staging_root: Some(staging_root.clone()),
                partitions,
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

    fn list_single_image(
        &self,
        path: &Path,
        options: &SafeFlashBuildOptions,
    ) -> Result<Vec<SafeFlashPartitionSource>, DomainError> {
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

        // 单独选中的镜像若正好只做假刷写（例如只挑了 system.img，或勾选了
        // 保留 ROOT 却挑了 boot.img）：文件本来就在本地，用它的真实大小计时。
        let simulated_flash_bytes = should_simulate_partition_flash(
            name,
            options.is_safe_flash,
            options.is_keep_root,
        )
        .then(|| std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0));

        Ok(vec![SafeFlashPartitionSource {
            partition_name: name.to_string(),
            image_path: path.to_string_lossy().into_owned(),
            has_slot: true,
            simulated_flash_bytes,
        }])
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

            if seen.insert(partition_name.clone()) {
                // 只做假刷写的分区（受保护分区、保留 ROOT 的启动分区）留在
                // 队列里：镜像已在本地，直接用它的真实大小计时，不写设备。
                let simulated_flash_bytes = should_simulate_partition_flash(
                    &partition_name,
                    options.is_safe_flash,
                    options.is_keep_root,
                )
                .then(|| path.metadata().map(|meta| meta.len()).unwrap_or(0));
                partitions.push(SafeFlashPartitionSource {
                    partition_name,
                    image_path: path.to_string_lossy().to_string(),
                    has_slot: true,
                    simulated_flash_bytes,
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
        // 固件包签名门禁：**解压任何镜像之前**校验 `.sig`。
        //
        // 这是整个 P2 里唯一真正有安全边界意义的校验：固件包来自磁盘/
        // 下载渠道,被替换成恶意镜像会直接写进设备(变砖或植入)。
        // 验签失败**直接拒绝**,不做任何"退回未校验内容"的降级。
        verify_firmware_package_signature(source)?;
        let mut archive = ZipArchive::new(File::open(source).map_err(|error| {
            DomainError::InvalidOperation(format!("打开固件压缩包失败：{error}"))
        })?)
        .map_err(|error| DomainError::InvalidFormat(format!("读取压缩包失败：{error}")))?;

        if has_payload_bin(&mut archive, cancellation)? {
            return Err(DomainError::InvalidFormat(
                "当前 Rust 版未内置 payload.bin 解包工具，请提供解包后的镜像目录或普通固件 zip。"
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

        let required_bytes = zip_extraction_output_size(&mut archive, cancellation)?;
        self.ensure_extraction_capacity(&output_dir, required_bytes)?;

        let mut partitions = Vec::new();
        let mut copied_bytes = 0u64;
        for index in 0..archive.len() {
            self.ensure_preparation_not_canceled(cancellation)?;
            let mut entry = archive.by_index(index).map_err(|error| {
                DomainError::InvalidFormat(format!("读取固件入口失败：{error}"))
            })?;
            let name = entry
                .name()
                .map_err(|error| {
                    DomainError::InvalidFormat(format!("读取固件入口名称失败：{error}"))
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

            // 「假戏真做」：只做假刷写的分区同样要真的解包到临时目录——
            // 解包进度、耗时与临时占用必须和真机刷写一致，唯一被模拟的是
            // 「写进设备」那一步。模拟耗时直接取落盘镜像的真实大小。
            let simulated_flash_bytes = should_simulate_partition_flash(
                &partition_name,
                options.is_safe_flash,
                options.is_keep_root,
            )
            .then(|| {
                std::fs::metadata(&output_path)
                    .map(|meta| meta.len())
                    .unwrap_or(0)
            });

            partitions.push(SafeFlashPartitionSource {
                partition_name,
                image_path: output_path.to_string_lossy().into_owned(),
                has_slot: true,
                simulated_flash_bytes,
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
                .map_err(|error| io::Error::other(format!("读取固件入口名称失败：{error}")))?
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
            .map_err(|error| DomainError::InvalidFormat(format!("读取固件入口失败：{error}")))?;
        let name = entry
            .name()
            .map_err(|error| DomainError::InvalidFormat(format!("读取固件入口名称失败：{error}")))?
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
        OtaDownloadError::Cancelled => DomainError::UserCancelled("固件下载已取消。".to_string()),
        OtaDownloadError::UnknownContentLength => {
            DomainError::InvalidOperation("固件下载失败：无法确定固件包大小。".to_string())
        }
        OtaDownloadError::InvalidInput(message)
        | OtaDownloadError::Download(message)
        | OtaDownloadError::Io(message) => {
            DomainError::InvalidOperation(format!("固件下载失败：{message}"))
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
            .map_err(|error| DomainError::InvalidFormat(format!("读取固件入口失败：{error}")))?;
        let name = entry.name().map_err(|error| {
            DomainError::InvalidFormat(format!("读取固件入口名称失败：{error}"))
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
            // 只做假刷写的分区也要真的解包（“假戏真做”），因此同样计入
            // 解包空间；只有该等式成立时落盘才算数。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FirmwareExtractEntry;
    use std::sync::{Arc, Barrier};

    /// 假刷写时长口径：`镜像大小 ÷ 35MB/s`。这是“看起来像真的在刷”的
    /// 唯一来源，必须精确到毫秒、且不许溢出。
    #[test]
    fn simulated_flash_duration_follows_the_35mbps_rule() {
        use super::simulated_flash::{duration, SPEED_BYTES_PER_SECOND};

        assert_eq!(SPEED_BYTES_PER_SECOND, 35 * 1024 * 1024);
        assert_eq!(duration(0), Duration::ZERO);
        assert_eq!(duration(SPEED_BYTES_PER_SECOND), Duration::from_secs(1));
        assert_eq!(
            duration(SPEED_BYTES_PER_SECOND / 2),
            Duration::from_millis(500)
        );
        // 典型 system.img（约 3.2GB）≈ 91 秒，量级与真机一致。
        assert_eq!(duration(3_200 * 1024 * 1024), Duration::from_millis(91_428));
        // u64::MAX 不得 panic（整型饱和）。
        assert!(duration(u64::MAX) > Duration::from_secs(1));
    }

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
            DomainError::InvalidOperation(message) if message.contains("无法确定固件包大小")
        ));
    }

    struct InertExecutor;

    impl CancellableProcessExecutor for InertExecutor {
        fn run(
            &self,
            _spec: ProcessCommand,
            _should_cancel: &mut dyn FnMut() -> bool,
        ) -> Result<ProcessOutput, DomainError> {
            Err(DomainError::Internal(
                "留痕资格测试不应真正执行进程".to_string(),
            ))
        }
    }

    /// 逐命令留痕只对真实系统执行器有意义。
    ///
    /// 若 `system()` 哪天被改回 `Self::new(Arc::new(SystemCancellableProcessExecutor))`，
    /// 生产会**静默**丢掉逐命令使用日志，而其余 safe_flash 测试仍会全绿
    /// ——所以这里把「谁有资格被套留痕层」钉住。
    #[test]
    fn only_the_system_executor_is_eligible_for_command_recording() {
        assert!(
            SafeFlashExecutionService::system().uses_system_executor(),
            "system() 必须自报 system-backed，否则生产不再逐命令留痕"
        );
        assert!(
            !SafeFlashExecutionService::new(Arc::new(InertExecutor)).uses_system_executor(),
            "调用方注入的执行器不得被套留痕层"
        );
        assert!(
            !SafeFlashExecutionService::system()
                .with_executor(Arc::new(InertExecutor))
                .uses_system_executor(),
            "换过执行器的服务不再视作 system-backed，避免重复包裹"
        );
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
    fn fastboot_failure_summary_redacts_credentials_and_url_userinfo() {
        let summary = fastboot_failure_summary(&ProcessOutput {
            exit_code: 1,
            stdout: "partition=boot token=token-secret-001\n".to_string(),
            stderr: "request=https://user-secret:password-secret@example.test/path\n".to_string(),
        });

        assert!(summary.contains("退出码 1"));
        for secret in ["token-secret-001", "user-secret", "password-secret"] {
            assert!(!summary.contains(secret), "failure summary leaked {secret}");
        }
        assert!(summary.contains("partition=boot"));
        assert!(summary.contains("https://[REDACTED]@example.test/path"));
    }

    #[test]
    fn fastboot_failure_summary_puts_each_stream_on_its_own_line() {
        let summary = fastboot_failure_summary(&ProcessOutput {
            exit_code: 1,
            stdout: "FAILED (remote: variable not found)".to_string(),
            stderr: "fastboot: error: Command failed".to_string(),
        });

        assert!(summary.contains("退出码 1。\nFAILED (remote: variable not found)"));
        assert!(summary.contains("\nfastboot: error: Command failed"));
    }

    #[test]
    fn fastboot_failure_summary_hides_private_keys_and_truncates_utf8_safely() {
        let private_key_summary = fastboot_failure_summary(&ProcessOutput {
            exit_code: 2,
            stdout: String::new(),
            stderr:
                "-----BEGIN PRIVATE KEY-----\nprivate-key-secret-001\n-----END PRIVATE KEY-----\n"
                    .to_string(),
        });
        assert!(private_key_summary.contains("[CREDENTIAL_REMOVED:PRIVATE_KEY]"));
        assert!(!private_key_summary.contains("private-key-secret-001"));
        assert!(!private_key_summary.contains("BEGIN PRIVATE KEY"));

        let long_utf8 = "分区写入失败🚀".repeat(500);
        let truncated = fastboot_failure_summary(&ProcessOutput {
            exit_code: 3,
            stdout: String::new(),
            stderr: long_utf8,
        });
        assert!(truncated.contains("日志已截断"));
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
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

/// 固件包签名门禁：在解压任何镜像之前校验旁挂的 `.sig`。
///
/// ## 策略（按调用来源区分）
///
/// - **本地用户选择的固件包**：要求存在 `<包>.sig` 且验签通过。
/// - **无签名**：拒绝，并给出明确指引。
///
/// 这里刻意**不**做"没有签名就放行"的降级——那等于把门禁变成可选项，
/// 攻击者只要删掉 `.sig` 就能绕过。fail-closed 才有意义。
///
/// ## 覆盖范围
///
/// 签名覆盖的是**固件包整体的 SHA-256**，不是单个镜像。因此一个签名能保护
/// 整包，且大包可以流式摘要（见 `verify_sha256_digest`）而不必整包读进内存。
fn verify_firmware_package_signature(source: &Path) -> Result<(), DomainError> {
    // 测试可注入公钥；发布构建不含该 feature，恒走编译期公钥。
    #[cfg(feature = "test-firmware-key-injection")]
    if let Some(key) = test_verifying_key() {
        return verify_firmware_package_signature_with_key(source, &key);
    }
    let verifying_key = nwflash_infrastructure::compiled_session_verifying_key().map_err(|_| {
        DomainError::Internal("编译期验证公钥缺失，无法校验固件包签名。".to_string())
    })?;
    verify_firmware_package_signature_with_key(source, &verifying_key)
}

/// 与 [`verify_firmware_package_signature`] 相同，但显式接收验证公钥。
///
/// 拆出这一层是为了让测试能注入测试公钥：生产构建的公钥来自编译期
/// `NWFLASH_SESSION_VERIFY_KEY_B64`（测试构建下为空），因此测试必须能
/// 替换它，否则所有涉及固件包的测试都会卡在“公钥缺失”上，反而掩盖了
/// 真正的验签逻辑。
fn verify_firmware_package_signature_with_key(
    source: &Path,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<(), DomainError> {
    use nwflash_protection::{verify_artifact_bytes, ArtifactVerificationError};

    let signature_path = firmware_signature_path(source);
    let signature = std::fs::read_to_string(&signature_path).map_err(|_| {
        DomainError::InvalidOperation(format!(
            "固件包缺少签名文件 {}：为安全起见已拒绝刷写。请使用官方渠道下载的固件包。",
            signature_path.display()
        ))
    })?;

    let package = std::fs::read(source)
        .map_err(|error| DomainError::InvalidOperation(format!("读取固件包失败：{error}")))?;

    verify_artifact_bytes(&package, &signature, verifying_key).map_err(|error| {
        let reason = match error {
            ArtifactVerificationError::MalformedSignature => "签名文件格式不合法",
            ArtifactVerificationError::InvalidSignatureLength => "签名长度不合法",
            ArtifactVerificationError::SignatureMismatch => {
                "签名与固件包内容不匹配（包可能被替换或损坏）"
            }
        };
        DomainError::InvalidOperation(format!(
            "固件包签名校验失败：{reason}。已拒绝刷写以免写入被篡改的镜像。"
        ))
    })
}
/// 由固件包 URL 推导签名 URL。
///
/// **必须插在路径末尾**，不能简单地对整串追加：`http://host:8080` 追加后
/// 会变成 `http://host:8080.sig`，`.sig` 落进端口位置，直接构造出非法 URL
/// （这正是实现过程中被在线路径测试抓出来的真实缺陷）。
///
/// 查询串要保留在 `.sig` **之后**：`.../rom.zip?v=2` -> `.../rom.zip.sig?v=2`。
fn firmware_signature_url(url: &str) -> String {
    let trimmed = url.trim();
    // 分离查询串：`.sig` 必须落在路径段上，查询串排在它之后。
    let (base, query) = match trimmed.split_once('?') {
        Some((base, query)) => (base, Some(query)),
        None => (trimmed, None),
    };
    // 关键：若 URL 没有路径段（形如 `http://host:port`），必须先补 `/`，
    // 否则 `.sig` 会紧贴在端口号后面，被解析成非法端口。
    let has_path_segment = base
        .split_once("://")
        .map(|(_, rest)| rest.contains('/'))
        .unwrap_or(false);
    let separator = if has_path_segment { "" } else { "/" };
    let stem = base.trim_end_matches('/');
    match query {
        Some(query) => format!("{stem}{separator}.sig?{query}"),
        None => format!("{stem}{separator}.sig"),
    }
}
/// 从固件包 URL 推导并下载它的 detached 签名文件。
///
/// 约定：签名地址是固件包地址 + `.sig`。下载失败时**不阻断**，
/// 而是留给后续的验签门禁去拒绝并给出统一文案——这样"缺签名"与
/// "签名不匹配"对用户呈现同一条处置指引，不会出现两套说辞。
async fn download_firmware_signature(
    url: &str,
    download_target: &Path,
    cancellation: &CancellationToken,
) -> Result<(), DomainError> {
    let signature_url = firmware_signature_url(url);
    let mut signature_path = download_target.as_os_str().to_os_string();
    signature_path.push(".sig");
    let signature_path = std::path::PathBuf::from(signature_path);

    download_to_file_with_cancellation(&signature_url, &signature_path, cancellation, None)
        .await
        .map_err(|error| {
            DomainError::InvalidOperation(format!(
                "下载固件包签名失败（{signature_url}）：{error}。已拒绝刷写以免写入未经校验的镜像。"
            ))
        })
        .map(|_bytes| ())
}
/// 测试用的固件包验签公钥注入点。
///
/// 生产构建下不存在（该 feature 不在发布构建里启用），因此这条路径
/// **无法**被发布二进制触发——它不会成为绕过验签的后门。
#[cfg(feature = "test-firmware-key-injection")]
fn test_verifying_key() -> Option<ed25519_dalek::VerifyingKey> {
    let encoded = std::env::var("NWFLASH_TEST_FIRMWARE_PUBLIC_KEY_B64").ok()?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded.trim())
        .ok()?;
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&bytes).ok()
}
/// 固件包的 detached 签名路径：`rom.zip` -> `rom.zip.sig`。
fn firmware_signature_path(source: &Path) -> std::path::PathBuf {
    let mut candidate = source.as_os_str().to_os_string();
    candidate.push(".sig");
    std::path::PathBuf::from(candidate)
}
