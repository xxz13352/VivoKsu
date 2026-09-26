//! Safe flash planning and source preparation utilities for the VIVO flashing workflow.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
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
        CancellableProcessExecutor, ProcessCommand, ProcessObservation, ProcessObserverError,
        ProcessOutput, ProcessOutputObserver, SystemCancellableProcessExecutor,
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

/// 单条分区刷写在**写入过程中**的进度观测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeFlashPartitionProgress {
    /// 正在写入的分区名。
    pub partition_name: String,
    /// 该分区的镜像总字节数（`0` 表示大小未知，此时只报「进行中」）。
    pub total_bytes: u64,
    /// 已估算写入的字节数。
    pub written_bytes: u64,
    /// 队列里的第几个分区（从 1 起）与总数，用于总进度换算。
    pub partition_index: usize,
    pub partition_total: usize,
}

pub type SafeFlashPartitionProgressSink = dyn Fn(SafeFlashPartitionProgress) + Send + Sync;

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
    /// 这一步刷写的目标分区名（仅分区刷写步骤有值）。
    ///
    /// 队列构造阶段就固定下来，日志与结构化展示同源：日志只记
    /// `刷写分区[i/n]`，分区名用于「当前分区」等展示，两处不再各自解析 argv。
    partition_name: Option<String>,
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
            partition_name: None,
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
            // 与 `command_detail.rs::output_reports_failure` 对齐：那里把 `error:`
            // 也列为 fastboot 的失败形态（其文档明确写了 `FAILED (...)`、
            // `ERROR ...`、`error:`、`remote error` 四种）。这里此前只看 `FAILED`
            // / `ERROR` 前缀与大写 `REMOTE ERROR`，于是 `fastboot: error: cannot
            // generate image` 这种**退出码 0** 的协议失败会被漏掉，接着往下刷下
            // 一个分区——正是这段扫描要防的"半刷状态机"。
            (line.starts_with("FAILED")
                || line.starts_with("ERROR")
                || line.to_ascii_uppercase().contains("REMOTE ERROR")
                || line.to_ascii_uppercase().contains("ERROR:"))
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

    /// 在底层执行器外面套一层「解析 fastboot 实时输出」的进度观测。
    ///
    /// 只有真实刷写才需要它：解析的是 fastboot 自己打印的 `Sending` /
    /// `<分区>: A KB/B KB` / `Writing` 行，得到的是**真实**传输量，而不是按
    /// 耗时推算的估算值。测试注入的假执行器不会被包装（它们的输出是固定
    /// 夹具，解析它们没有意义，而且会破坏既有的执行器断言）。
    ///
    /// 返回包装后的服务与观测器句柄：句柄要交给执行循环，在每次刷写前登记
    /// 当前分区的上下文。
    pub fn with_fastboot_output_progress(
        self,
        sink: Arc<SafeFlashPartitionProgressSink>,
    ) -> (Self, Option<Arc<FastbootProgressExecutor>>) {
        // 只有**真实系统执行器**才包：包装会把 flash 命令改走
        // `run_command_with_cancel_observed`，而那条路径每次调用都会起一个
        // 观测线程并在返回前 join。测试注入的假执行器输出是固定夹具，解析
        // 它们没有意义，却要为此付出一次线程派发/排空——漏掉这层判断会让
        // 每个伪造 flash 的用例都卡在观测线程排空上。
        if !self.system_executor {
            return (self, None);
        }
        let observer = Arc::new(FastbootProgressExecutor::new(self.executor.clone(), sink));
        let service = Self {
            executor: observer.clone(),
            system_executor: false,
            ..self
        };
        (service, Some(observer))
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
        self.execute_with_partition_progress_internal(
            request,
            &mut is_canceled,
            &mut report_stage,
            &mut report_progress,
            &mut on_partition_failure,
            &mut is_suspended,
            None,
            None,
            None,
        )
    }

    /// 与 [`Self::execute_with_suspend_gate`] 相同，另外接受一个**分区写入进度**
    /// 观测回调。
    ///
    /// 为什么需要它：`fastboot flash <分区> <镜像>` 是一次阻塞调用，大分区
    /// （system/product 动辄数百 MB）在真机上要写几十秒到几分钟。只按命令
    /// 边界上报进度的话，整段时间界面完全静止。`fastboot.exe` 在写入期间
    /// 不输出可用进度，因此这里**不依赖子进程输出**，改用两条真实信号合成：
    ///
    /// - 假刷写分区：按 `已等待时长 / 模拟总时长` 精确换算字节数（见
    ///   `simulated_flash::duration`（私有模块，故不做 rustdoc 链接）），进度与真机一致。
    /// - 真实分区：按已耗时相对「该镜像在 35MB/s 下的参考耗时」估算写入量。
    ///   估算值只用于展示，**绝不影响**成功判定与取消语义；上限锁死在
    ///   镜像大小的 95%，命令真正返回后才补到 100%，避免进度条先跑满再等待。
    #[allow(clippy::too_many_arguments)]
    pub fn execute_with_partition_progress<F, S, P, D, G>(
        &self,
        request: SafeFlashExecutionRequest<'_>,
        mut is_canceled: F,
        mut report_stage: S,
        mut report_progress: P,
        mut on_partition_failure: Option<D>,
        mut is_suspended: G,
        partition_progress: Option<&Arc<SafeFlashPartitionProgressSink>>,
        partition_observer: Option<&FastbootProgressExecutor>,
        // 逐分区进度的**状态行**通道：只更新界面文案，不写本地操作日志。
        // 与 `report_stage` 分开是因为 VIVO 线刷的 `刷写分区[i/n]` 要显示在
        // 状态行与进度行里，但不该按分区往日志区刷流水。
        report_partition_stage: Option<&mut dyn FnMut(String)>,
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
        self.execute_with_partition_progress_internal(
            request,
            &mut is_canceled,
            &mut report_stage,
            &mut report_progress,
            &mut on_partition_failure,
            &mut is_suspended,
            partition_progress,
            partition_observer,
            report_partition_stage,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_with_partition_progress_internal<F, S, P, D, G>(
        &self,
        request: SafeFlashExecutionRequest<'_>,
        mut is_canceled: &mut F,
        report_stage: &mut S,
        report_progress: &mut P,
        on_partition_failure: &mut Option<D>,
        is_suspended: &mut G,
        partition_progress: Option<&Arc<SafeFlashPartitionProgressSink>>,
        partition_observer: Option<&FastbootProgressExecutor>,
        mut report_partition_stage: Option<&mut dyn FnMut(String)>,
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
        // 每个分区镜像的落盘大小：真刷写时用来估算写入百分比。按分区基名
        // 建表（`system_a` / `system_b` 共用 `system` 的镜像），查不到就退化为
        // 「大小未知」，只报进行中而不猜百分比。
        let prepared_image_sizes = request
            .source
            .partitions
            .iter()
            .filter_map(|partition| {
                let bytes = partition.simulated_flash_bytes?;
                Some((partition.partition_name.clone(), bytes))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let prepared_image_sizes = &prepared_image_sizes;
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
                .map_err(|error| DomainError::InvalidOperation(format!("重启设备失败：{error}")))?;
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
                    partition_name: Some(target),
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
                transport
                    .build_fastboot_reboot_command(&serial)
                    .map_err(|error| {
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
                partition_name,
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
                // 分区名在队列构造阶段就已固定（旧行为从 argv 反查，日志与
                // 失败提示两条来源可能分叉）；这里只做缺失兜底。
                last_partition_target = partition_name.clone().unwrap_or_default();
                partition_index += 1;
                // 只报「正在写第几个分区」，且**只进状态行、不进本地日志**：
                // 界面需要 i/n 来定位进度，但逐分区的流水会把日志区淹掉
                // （用户看日志是看"发生了什么"）。分区名本来就不在这条文案里，
                // 点此不会泄漏"当前在刷哪个分区"。
                let partition_line = format!("刷写分区[{partition_index}/{partition_total}]");
                match report_partition_stage.as_deref_mut() {
                    // 有专用通道：只更新状态行，不写日志。
                    Some(emit) => emit(partition_line),
                    // 没接线时退回原语义（写日志），保证既有调用点行为不变。
                    None => report_stage(partition_line),
                }
            } else if tolerate_failure {
                // 重启到 REC：先把「进 REC 后要手动做什么」写给用户，
                // 设备进入 REC 后程序就再也探测不到它了。
                report_stage(SAFE_FLASH_WIPE_DATA_MANUAL_STEPS.to_string());
            }
            report_progress(index as f64 / command_total as f64);
            // 分区写入期间的实时进度：整条 `fastboot flash` 是阻塞调用，没有
            // 它界面会在整个写入过程里静止。假刷写按真实等待时长换算，真刷写
            // 按已耗时相对参考耗时估算（估算只影响展示，不影响成功判定）。
            let partition_bytes = if is_partition_flash {
                partition_image_bytes(simulated_flash_bytes, &command, prepared_image_sizes)
            } else {
                None
            };
            // 分区上下文（名字/大小/序号）只算一次：真实输出解析与耗时估算
            // 两条通路共用它，避免两处各自推导而出现不一致的分区名。
            let partition_update = if is_partition_flash {
                partition_bytes.map(|total_bytes| SafeFlashPartitionProgress {
                    partition_name: last_partition_target.clone(),
                    total_bytes,
                    written_bytes: 0,
                    partition_index,
                    partition_total,
                })
            } else {
                None
            };
            if let (Some(observer), Some(update)) = (partition_observer, partition_update.clone()) {
                // 真实输出解析：登记分区上下文，让解析出的字节数能对应到
                // 「第几个分区的哪个镜像」。
                observer.begin_partition(update);
            }
            let mut report_partition_tick = |written_bytes: u64| {
                let (Some(sink), Some(mut update)) = (partition_progress, partition_update.clone())
                else {
                    return;
                };
                update.written_bytes = written_bytes.min(update.total_bytes);
                sink(update);
            };
            let run_result = if let Some(bytes) = simulated_flash_bytes {
                // 受保护分区：只等时间，不发命令，日志与真实刷写一字不差。
                self.run_simulated_flash(bytes, &mut is_canceled, &mut report_partition_tick)
            } else if is_partition_flash {
                self.run_partition_flash(command, &mut is_canceled, &mut report_partition_tick)
            } else {
                self.run_required(command, &mut is_canceled, "fastboot 命令")
            };
            match run_result {
                Ok(_) => {
                    executed_command_count += 1;
                    if is_flash {
                        flashed_partition_count += 1;
                    }
                    // 命令已返回 = 这个分区确实写完了：补最后一格到 100%。
                    //
                    // 真实分区的写入量是**估算**的（fastboot 写入期间不输出可用
                    // 进度），在上限处锁在 95%，把余量留给这一刻；假刷写本身就
                    // tick 到满，这里再补一次是幂等的。少了这一步，「当前分区」
                    // 进度条会在最后一格永远停住、看不到完成。
                    if is_partition_flash {
                        if let Some(total_bytes) = partition_bytes {
                            report_partition_tick(total_bytes);
                        }
                    }
                }
                Err(error) => {
                    // 主动停止/会话取消不是分区刷写失败：直接透传取消，
                    // 避免在取消收尾期间误触发前端失败决策弹窗。
                    if matches!(error, DomainError::UserCancelled(_)) {
                        return Err(error);
                    }
                    // 失败不再改写那一行：进度行只表示「正在写第几个分区」，
                    // 失败原因由分区失败弹窗与随后的错误日志承担。
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
    fn run_partition_flash<F, T>(
        &self,
        command: ProcessCommand,
        is_canceled: &mut F,
        mut report_tick: T,
    ) -> Result<ProcessOutput, DomainError>
    where
        F: FnMut() -> bool,
        T: FnMut(u64),
    {
        self.ensure_not_canceled(is_canceled)?;
        // `fastboot flash` 是阻塞调用，且 fastboot.exe 在写入期间不输出可用
        // 进度，因此真实分区的写入量只能**估算**：按已耗时相对「该镜像在
        // 35MB/s 下的参考耗时」折算。上限锁在总大小的 95%，命令返回后才由
        // 调用方补满——否则进度条会先跑满、再干等命令结束。
        let estimated_total = command
            .args
            .last()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .filter(|bytes| *bytes > 0);
        let reference = estimated_total.map(simulated_flash::duration);
        let started = std::time::Instant::now();
        let mut is_canceled_with_tick = || {
            if let (Some(total), Some(reference)) = (estimated_total, reference) {
                if !reference.is_zero() {
                    let elapsed = started.elapsed().as_millis() as u64;
                    let ratio = (elapsed.min(reference.as_millis() as u64) as f64)
                        / (reference.as_millis().max(1) as f64);
                    // 95% 封顶：留给「命令真正返回」那一下补齐。
                    report_tick((total as f64 * ratio * 0.95) as u64);
                }
            }
            is_canceled()
        };
        let output = self
            .executor
            .run_with_timeout(
                command,
                Some(command_budget::FLASH),
                &mut is_canceled_with_tick,
            )
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
                    &output, &failure,
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
            let probe = self.tools.fastboot_devices_command().map_err(|error| {
                DomainError::InvalidOperation(format!("检测 fastbootd 失败：{error}"))
            });
            let output = match probe
                .and_then(|command| self.run_required(command, is_canceled, "检测 fastbootd"))
            {
                Ok(output) => output,
                Err(DomainError::UserCancelled(_)) => {
                    return Err(DomainError::UserCancelled("运行被用户取消".to_string()))
                }
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
                            return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
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
        let total_millis = (self.fastbootd_attempts as u64)
            .saturating_mul(self.fastbootd_poll_interval.as_millis() as u64);
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
    fn run_simulated_flash<F, T>(
        &self,
        bytes: u64,
        is_canceled: &mut F,
        mut report_tick: T,
    ) -> Result<ProcessOutput, DomainError>
    where
        F: FnMut() -> bool,
        T: FnMut(u64),
    {
        self.ensure_not_canceled(is_canceled)?;
        let total = simulated_flash::duration(bytes);
        let total_ms = total.as_millis() as u64;
        if total_ms == 0 {
            // `duration` 是整型毫秒截断：小于约 36KB 的镜像会算出 **0ms**，
            // 于是下面的等待循环一次都不执行、一个 tick 都不发，「当前分区」
            // 进度条在这类镜像上根本不会动。这里如实上报整张镜像已写完——
            // 按定义它就是"瞬间写完"。
            report_tick(bytes);
            self.ensure_not_canceled(is_canceled)?;
            return Ok(ProcessOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            });
        }
        let mut remaining = total;
        while !remaining.is_zero() {
            let slice = remaining.min(simulated_flash::SLICE);
            thread::sleep(slice);
            remaining -= slice;
            // 假刷写的写入量是**精确**的：等待时长本就按 `字节数 ÷ 35MB/s`
            // 折算而来，因此按已等待比例还原字节数即可，不需要估算。
            let written = bytes.saturating_mul((total - remaining).as_millis() as u64) / total_ms;
            report_tick(written);
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

/// 解析这一步要写入的镜像总字节数。
///
/// 优先用假刷写已记录的精确大小（`simulated_flash_bytes` 取的就是落盘镜像的
/// 真实长度）；真刷写时从命令的镜像路径读一次 `fs::metadata`，失败则尝试用
/// 分区源表里的同名字段兜底。都拿不到就返回 `None`——宁可只显示「进行中」，
/// 也不编一个假的总量出来。
/// 在真实进程执行器外面套一层：把 `fastboot flash` 的**实时输出**解析成
/// 字节进度。
///
/// 这是「真刷写也有真进度」的关键。此前只能按耗时估算，因为假设 fastboot
/// 写入期间不输出可用进度；而本项目使用的 fastboot 带实时回调，会逐行打印
/// `Sending 'x' (N KB)...` / `x: A KB/B KB` / `Writing 'x'...`，解析这些行
/// 得到的是**真实**传输量，比估算准得多。
///
/// 设计取舍：只包刷写类命令，其余命令原样透传（`devices`/`getvar` 的输出没有
/// 进度语义，包了只是白付一次解析开销）。估算逻辑保留为兜底——fastboot 版本
/// 不同、输出被重定向或静默模式下解析不到任何行时，界面仍有进度可看。
pub struct FastbootProgressExecutor {
    inner: Arc<dyn CancellableProcessExecutor>,
    sink: Arc<SafeFlashPartitionProgressSink>,
    /// 当前分区名与序号，由调用方在每次刷写前更新。
    context: Arc<Mutex<FastbootProgressContext>>,
}

#[derive(Debug, Default)]
struct FastbootProgressContext {
    partition_name: String,
    total_bytes: u64,
    partition_index: usize,
    partition_total: usize,
}

impl FastbootProgressExecutor {
    pub fn new(
        inner: Arc<dyn CancellableProcessExecutor>,
        sink: Arc<SafeFlashPartitionProgressSink>,
    ) -> Self {
        Self {
            inner,
            sink,
            context: Arc::new(Mutex::new(FastbootProgressContext::default())),
        }
    }

    /// 刷写下一个分区前登记上下文（分区名、镜像大小、序号）。
    pub fn begin_partition(&self, update: SafeFlashPartitionProgress) {
        if let Ok(mut context) = self.context.lock() {
            context.partition_name = update.partition_name.clone();
            context.total_bytes = update.total_bytes;
            context.partition_index = update.partition_index;
            context.partition_total = update.partition_total;
        }
        // 立刻上报一次 0，让 UI 在写入开始时就切到新分区，而不是等第一行输出。
        (self.sink)(update);
    }
}

impl CancellableProcessExecutor for FastbootProgressExecutor {
    fn run(
        &self,
        spec: ProcessCommand,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError> {
        self.run_with_timeout(spec, None, should_cancel)
    }

    fn run_with_timeout(
        &self,
        spec: ProcessCommand,
        timeout: Option<Duration>,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError> {
        let is_flash = spec
            .args
            .iter()
            .any(|argument| argument.eq_ignore_ascii_case("flash"));
        if !is_flash {
            return self.inner.run_with_timeout(spec, timeout, should_cancel);
        }
        let image_bytes = spec
            .args
            .last()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let observer = Arc::new(FastbootProgressObserver {
            parser: Mutex::new(FastbootProgressParser::new(image_bytes)),
            sink: self.sink.clone(),
            context: self.context.clone(),
        });
        let outcome = nwflash_windows::process::run_command_with_cancel_observed(
            spec,
            timeout,
            should_cancel,
            observer,
        );
        outcome.result
    }
}

/// 把 fastboot 的逐行输出喂给解析器，再把解析出的字节数转成进度观测。
struct FastbootProgressObserver {
    parser: Mutex<FastbootProgressParser>,
    sink: Arc<SafeFlashPartitionProgressSink>,
    context: Arc<Mutex<FastbootProgressContext>>,
}

impl ProcessOutputObserver for FastbootProgressObserver {
    fn observe(&self, observation: ProcessObservation<'_>) -> Result<(), ProcessObserverError> {
        // 命令结束：把最后一块**未报满的余量**补上。
        //
        // fastboot 的进度行是按「块」打印的，最后一块很可能只打了
        // `Writing 'X'...` 或是直接以 `OKAY` 收尾，于是解析器手里的累计字节数
        // 会停在这一块的中途（实测能停在 ~60%）。这样直到 `Finished.` 才补齐，
        // 而有些版本/重定向下根本没有 `Finished.` 行，进度条就永远停在半途。
        // `finish()` 不臆造进度：失败时返回 `None`，成功时只补到本地镜像长度。
        if let ProcessObservation::Finished(metadata) = observation {
            // 只有真正成功才补满：`exit_code == 0` 且进程正常结束。取消、超时、
            // 输出超限都以非 0 退出码或非 `Completed` 的 termination 收尾，此时
            // 补满会把被打断的刷写谎报成 100%。
            let succeeded = metadata.exit_code == Some(0)
                && matches!(
                    metadata.termination,
                    nwflash_windows::process::ProcessTermination::Completed
                );
            if succeeded {
                if let Some(written_bytes) = self.parser.lock().ok().and_then(|p| p.finish()) {
                    self.report(written_bytes);
                }
            }
            return Ok(());
        }
        let ProcessObservation::Output { bytes, .. } = observation else {
            return Ok(());
        };
        // 进度行都是 ASCII；用有损解码即可，非法字节会被替换而不会 panic。
        let text = String::from_utf8_lossy(bytes);
        let Ok(mut parser) = self.parser.lock() else {
            return Ok(());
        };
        let mut reported = None;
        for line in text.lines() {
            if let Some(bytes) = parser.observe_line(line.trim_end()) {
                reported = Some(bytes);
            }
        }
        drop(parser);
        if let Some(written_bytes) = reported {
            self.report(written_bytes);
        }
        Ok(())
    }
}

impl FastbootProgressObserver {
    fn report(&self, written_bytes: u64) {
        let Ok(context) = self.context.lock() else {
            return;
        };
        (self.sink)(SafeFlashPartitionProgress {
            partition_name: context.partition_name.clone(),
            total_bytes: context.total_bytes,
            // 按分区总长封顶。原文案是
            // `written_bytes.min(context.total_bytes.max(written_bytes))`，
            // 那个 `.max(written_bytes)` 让内层恒 ≥ written_bytes、外层恒取
            // written_bytes —— 整个表达式等价于**原样放行**，是个看着像封顶、
            // 实际什么都没做的假保护。解析器的总量取自**本命令镜像**，而这里
            // 的总量取自**分区镜像表**，两者不一致时（例如镜像被换过、或块数与
            // 本地大小有出入）written 会越过 total，让下游拿到 >100% 的进度。
            written_bytes: written_bytes.min(context.total_bytes),
            partition_index: context.partition_index,
            partition_total: context.partition_total,
        });
    }
}
/// fastboot.exe 在刷写期间打印的实时进度行解析。
///
/// 带进度回调的 fastboot（本项目与参考实现用的是同一份）会在传输过程中按行
/// 输出形如：
///
/// ```text
/// Sending 'system' (393216 KB)...
/// Sending sparse 'system' (65536 KB)...
/// system: 32768 KB/65536 KB
/// Writing 'system'...
/// Finished. Total time: 12.345s
/// ```
///
/// 关键点（都是踩过的坑）：
///
/// - **大镜像会被切成多个 sparse 块**。`Writing '...'` 只代表**当前块**写完，
///   不能一见它就记 100%，否则第一块结束进度条就满了。
/// - 因此按「块」累计：`Sending` 行给出本块大小并重置块内计数，进度行的
///   `已传/总量` 折算块内增量，`Writing` 行把本块**未报满的余量**补齐。
/// - 累计值以**本地镜像大小**为上限，避免 fastboot 报的块大小之和与实际
///   镜像长度有出入时越界。
/// - 出现 `FAILED` / `error` 后不再臆造进度（失败要如实反映）。
#[derive(Debug, Default)]
pub struct FastbootProgressParser {
    /// 本地镜像字节数（`0` 表示未知）。
    command_total_bytes: u64,
    /// 当前 sparse 块的期望字节数与已上报字节数。
    chunk_expected_bytes: u64,
    chunk_reported_bytes: u64,
    /// 本命令已累计上报的字节数。
    accumulated_bytes: u64,
    failed: bool,
}

impl FastbootProgressParser {
    /// `image_bytes` 为本次要写入的本地镜像长度；未知时传 `0`。
    pub fn new(image_bytes: u64) -> Self {
        Self {
            command_total_bytes: image_bytes,
            ..Self::default()
        }
    }

    /// 解析一行输出，返回**本次应当上报的累计字节数**（无进展时为 `None`）。
    pub fn observe_line(&mut self, line: &str) -> Option<u64> {
        if line.contains("FAILED") || contains_word_error(line) {
            self.failed = true;
        }

        let mut reported = None;

        if let Some(sent_bytes) = parse_sending_bytes(line) {
            // 新块开始：镜像总长优先用本地真实大小，拿不到才退回收 fastboot
            // 自报的块大小（单块镜像时两者等价）。
            if self.command_total_bytes == 0 {
                self.command_total_bytes = sent_bytes;
            }
            self.chunk_expected_bytes = sent_bytes;
            self.chunk_reported_bytes = 0;
        }

        if let Some(current_bytes) = parse_progress_bytes(line) {
            let current_bytes = if self.chunk_expected_bytes > 0 {
                current_bytes.min(self.chunk_expected_bytes)
            } else if self.command_total_bytes > 0 {
                current_bytes.min(self.command_total_bytes)
            } else {
                current_bytes
            };
            // 进度行是块内**累计**值；回退（换行/重排）时按「本块从头」重算，
            // 绝不让累计量倒退。
            let delta = current_bytes.saturating_sub(self.chunk_reported_bytes);
            let delta = if current_bytes < self.chunk_reported_bytes {
                current_bytes
            } else {
                delta
            };
            if delta > 0 {
                self.accumulate(delta);
                self.chunk_reported_bytes = current_bytes;
                reported = Some(self.accumulated_bytes);
            }
        }

        // `Writing '<分区>'...` 表示当前块传完并落盘：把本块还没报满的余量补上。
        // 只在当前块确实有未报满余量时补，避免把多块镜像的第一个 Writing
        // 直接当成整张镜像完成。
        if !self.failed
            && line.contains("Writing '")
            && self.chunk_expected_bytes > self.chunk_reported_bytes
        {
            let remainder = self.chunk_expected_bytes - self.chunk_reported_bytes;
            self.accumulate(remainder);
            self.chunk_reported_bytes = self.chunk_expected_bytes;
            reported = Some(self.accumulated_bytes);
        }

        // `Finished. Total time: ...` 是命令收尾：补齐到总大小。
        if !self.failed && line.starts_with("Finished.") && self.command_total_bytes > 0 {
            self.accumulated_bytes = self.command_total_bytes;
            self.chunk_reported_bytes = self.chunk_expected_bytes;
            reported = Some(self.accumulated_bytes);
        }

        reported
    }

    fn accumulate(&mut self, delta: u64) {
        self.accumulated_bytes = self.accumulated_bytes.saturating_add(delta);
        if self.command_total_bytes > 0 {
            self.accumulated_bytes = self.accumulated_bytes.min(self.command_total_bytes);
        }
    }

    /// 命令**成功**结束时应当上报的最终字节数：把最后一块没报满的余量补上，
    /// 失败时返回 `None`（绝不臆造进度）。
    ///
    /// 由 `FastbootProgressObserver`（私有类型）在 `ProcessObservation::Finished` 上调用，
    /// 这是「最后一块没有被 `Writing` / `Finished.` 行报满」时的唯一收口。
    ///
    /// 判「成功」必须看命令的真实结局，不能只看输出里有没有 `FAILED`：取消、
    /// 超时、输出超限触发终止时 fastboot 往往还没打出任何 `FAILED` 行，只看输出
    /// 就会把一次被打断的刷写谎报成 100%。因此成败由调用方按退出码/终止原因
    /// 判定，这里再叠一层输出侧证据。
    pub fn finish(&self) -> Option<u64> {
        if self.failed {
            return None;
        }
        let total = self.command_total_bytes;
        if total == 0 {
            return None;
        }
        (self.accumulated_bytes < total).then_some(total)
    }
}

/// `Sending 'system' (393216 KB)...` / `Sending sparse 'system' (65536 KB)...`
fn parse_sending_bytes(line: &str) -> Option<u64> {
    let start = line.find("Sending")? + "Sending".len();
    let rest = &line[start..];
    let open = rest.find('(')?;
    let close = rest[open..].find(')')? + open;
    parse_size_token(&rest[open + 1..close])
}

/// `system: 32768 KB/65536 KB` → 已传字节数。
fn parse_progress_bytes(line: &str) -> Option<u64> {
    // 形如 `<名称>: <已传>/<总量>`；名称不含空格（分区名或 fastboot 的标识）。
    let colon = line.find(':')?;
    if line[..colon].contains(' ') {
        return None;
    }
    let payload = line[colon + 1..].trim();
    let (current, _rest) = payload.split_once('/')?;
    parse_size_token(current)
}

/// 解析 `393216 KB` / `12.5 MB` / `1024 B` 这类「数值 + 单位」片段。
fn parse_size_token(token: &str) -> Option<u64> {
    let token = token.trim();
    let split = token.find(|character: char| !(character.is_ascii_digit() || character == '.'))?;
    let (number, unit) = token.split_at(split);
    let value = number.parse::<f64>().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let multiplier = match unit.trim().to_ascii_uppercase().as_str() {
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "MB" => 1024.0 * 1024.0,
        "KB" => 1024.0,
        "B" => 1.0,
        _ => return None,
    };
    Some((value * multiplier) as u64)
}

/// 匹配独立的 `error` 单词（避免把分区名里的子串误判成失败）。
///
/// 分词时把 `_` 当作词内字符：分区名形如 `error_log`、`system_a`，若按
/// 「非字母数字即分隔符」切分，`error_log` 会被切成 `error` + `log`，于是一个
/// 完全正常的分区名就被误判成刷写失败。参考实现用的 `\berror\b` 正则有同样的
/// 问题（`_` 在 .NET 里算词字符，`\b` 不会在这里断开；但 `error-log` 之类仍会）。
fn contains_word_error(line: &str) -> bool {
    line.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|word| word.eq_ignore_ascii_case("error"))
}

fn partition_image_bytes(
    simulated_flash_bytes: Option<u64>,
    command: &ProcessCommand,
    prepared_image_sizes: &std::collections::HashMap<String, u64>,
) -> Option<u64> {
    if let Some(bytes) = simulated_flash_bytes {
        return Some(bytes);
    }
    // `fastboot flash <分区> <镜像>`：镜像恒为最后一个参数。
    if let Some(image_path) = command.args.last() {
        if let Ok(metadata) = std::fs::metadata(image_path) {
            if metadata.is_file() && metadata.len() > 0 {
                return Some(metadata.len());
            }
        }
    }
    command
        .args
        .iter()
        .find_map(|argument| prepared_image_sizes.get(argument).copied())
        .filter(|bytes| *bytes > 0)
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
            SafeFlashSource::Online { url, pd, version } => {
                self.resolve_online_source(
                    &url,
                    &pd,
                    &version,
                    options,
                    cancellation,
                    download_progress,
                    preparation_progress.as_ref(),
                )
                .await
            }
        }
    }

    pub fn resolve_payload_source(
        &self,
        payload_source: &Path,
        options: &SafeFlashBuildOptions,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.resolve_payload_source_with_cancellation(
            payload_source,
            options,
            &CancellationToken::new(),
        )
    }

    pub fn resolve_payload_source_with_cancellation(
        &self,
        payload_source: &Path,
        options: &SafeFlashBuildOptions,
        cancellation: &CancellationToken,
    ) -> Result<SafeFlashPreparedSource, DomainError> {
        self.resolve_payload_source_with_cancellation_and_progress(
            payload_source,
            options,
            cancellation,
            None,
        )
    }

    pub fn resolve_payload_source_with_cancellation_and_progress(
        &self,
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
                Path::new(""),
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
            // 进度回调要被心跳线程共享（`Send + 'static`），因此这里必须把
            // 需要的一切**按值**搬进闭包：借用 `preparation_progress` /
            // `payload_output_bytes` 会被生命周期挡住。
            let preparation_progress_for_extraction = preparation_progress.cloned();
            let images = FirmwareExtractService::extract_payload_with_expected_sizes_and_progress(
                Path::new(""),
                payload_source,
                &selected,
                &image_directory,
                || cancellation.is_cancelled(),
                move |_, written_bytes| {
                    report_preparation_progress(
                        preparation_progress_for_extraction.as_ref(),
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
                // payload 由进程内解析器处理，不再需要外部可执行文件。
                let prepared = self.resolve_payload_source_with_cancellation_and_progress(
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
        let simulated_flash_bytes =
            should_simulate_partition_flash(name, options.is_safe_flash, options.is_keep_root)
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

#[cfg(test)]
mod fastboot_progress_tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    #[test]
    fn sending_line_reports_block_size_and_units_are_converted() {
        assert_eq!(
            parse_sending_bytes("Sending 'system' (393216 KB)..."),
            Some(393216 * 1024)
        );
        assert_eq!(
            parse_sending_bytes("Sending sparse 'system' (65536 KB)..."),
            Some(65536 * 1024)
        );
        assert_eq!(
            parse_sending_bytes("Sending 'boot' (12.5 MB)..."),
            Some(13_107_200)
        );
        // 非 Sending 行不得误报。
        assert_eq!(parse_sending_bytes("Writing 'boot'..."), None);
    }

    #[test]
    fn progress_line_parses_transferred_bytes_without_spaces_on_the_name() {
        assert_eq!(
            parse_progress_bytes("system: 32768 KB/65536 KB"),
            Some(32768 * 1024)
        );
        assert_eq!(parse_progress_bytes("boot: 1024 KB/4096 KB"), Some(MIB));
        // 名称里带空格（例如某些包裹输出）不应当被当成进度行。
        assert_eq!(parse_progress_bytes("Sending 'boot' (4096 KB)"), None);
    }

    #[test]
    fn progress_never_goes_backwards_and_caps_at_the_local_image_size() {
        let mut parser = FastbootProgressParser::new(8 * MIB);
        // 真实顺序：先 Sending 报块大小，再逐行报块内进度。
        assert_eq!(parser.observe_line("Sending 'boot' (8192 KB)..."), None);
        let first = parser
            .observe_line("boot: 2048 KB/8192 KB")
            .expect("progress expected");
        let second = parser
            .observe_line("boot: 4096 KB/8192 KB")
            .expect("progress expected");
        assert!(second > first, "进度必须单调推进：{first} -> {second}");
        // 块内计数回退时按「本块从头」重算，但累计量绝不倒退。
        let after_regress = parser
            .observe_line("boot: 1024 KB/8192 KB")
            .expect("regression must still report");
        assert!(
            after_regress >= second,
            "累计量不得倒退：{second} -> {after_regress}"
        );
        // 超过本地镜像大小的上报要被夹住。
        let capped = parser
            .observe_line("boot: 999999 KB/999999 KB")
            .expect("progress expected");
        assert!(
            capped <= 8 * MIB,
            "上报量不得超过本地镜像大小：{capped} > {}",
            8 * MIB
        );
    }

    #[test]
    fn first_writing_line_of_a_multi_chunk_image_does_not_jump_to_full() {
        // 关键回归：大镜像被切成多个 sparse 块，`Writing` 只代表**当前块**完成。
        // 旧实现一见 Writing 就当整张镜像完成，导致进度条第一块就满。
        let total = 128 * MIB;
        let mut parser = FastbootProgressParser::new(total);

        // 第一块：64MiB 传完。进度行本身就把本块报满，因此 `Writing` 不再产生
        // 新的上报——这正确，关键是不能报成整张镜像。
        parser.observe_line("Sending sparse 'system' (65536 KB)...");
        let after_first_chunk = parser
            .observe_line("system: 65536 KB/65536 KB")
            .expect("first chunk progress must report");
        assert_eq!(after_first_chunk, 64 * MIB);
        assert!(
            after_first_chunk < total,
            "第一块完成时绝不能报满整张镜像：{after_first_chunk} vs {total}"
        );
        assert_eq!(
            parser.observe_line("Writing 'system'..."),
            None,
            "本块已在进度行报满，Writing 不应重复上报"
        );

        // 第二块：再来 64MiB。累计必须跨块推进到整张镜像。
        parser.observe_line("Sending sparse 'system' (65536 KB)...");
        let after_second_chunk = parser
            .observe_line("system: 65536 KB/65536 KB")
            .expect("second chunk progress must report");
        assert_eq!(after_second_chunk, total, "两块后必须累计到整张镜像");
    }

    #[test]
    fn writing_line_fills_the_unreported_remainder_of_the_current_chunk() {
        // 有些 fastboot 版本在块尾不再打进度行，直接 Writing。此时必须把
        // 本块余量补齐，否则进度会一直停在半途。
        let mut parser = FastbootProgressParser::new(4 * MIB);
        parser.observe_line("Sending 'boot' (4096 KB)...");
        parser.observe_line("boot: 1024 KB/4096 KB");
        let filled = parser
            .observe_line("Writing 'boot'...")
            .expect("remainder must be filled");
        assert_eq!(filled, 4 * MIB);
    }

    #[test]
    fn finished_line_completes_the_command_without_inventing_progress_on_failure() {
        // 成功：Finished 补齐到整张镜像。
        let mut parser = FastbootProgressParser::new(6 * MIB);
        parser.observe_line("Sending 'vendor_boot' (6144 KB)...");
        let finished = parser
            .observe_line("Finished. Total time: 3.210s")
            .expect("finished must complete");
        assert_eq!(finished, 6 * MIB);

        // 失败：不得再臆造进度。
        let mut failed = FastbootProgressParser::new(6 * MIB);
        failed.observe_line("Sending 'vendor_boot' (6144 KB)...");
        failed.observe_line("FAILED (remote: 'write failed')");
        assert_eq!(failed.observe_line("Writing 'vendor_boot'..."), None);
        assert_eq!(failed.observe_line("Finished. Total time: 1.0s"), None);
        assert_eq!(failed.finish(), None);
    }

    #[test]
    fn observer_fills_the_last_chunk_remainder_when_the_command_finishes() {
        // 回归：fastboot 的进度行按「块」打印，最后一块常常只打 `Writing 'X'...`
        // 或直接以 `OKAY` 收尾，解析器手里的累计字节数会停在这一块的中途。
        // `FastbootProgressParser::finish()` 本来是唯一的收口，但**生产路径从没
        // 调用过它**（只有测试调），于是「当前分区」进度条在命令成功返回后仍停在
        // 半途。这条用例驱动真实的 observer，要求 Finished 事件把余量补满。
        let total = 128 * MIB;
        let reported = Arc::new(Mutex::new(Vec::<u64>::new()));
        let collected = Arc::clone(&reported);
        let sink: Arc<SafeFlashPartitionProgressSink> = Arc::new(move |progress| {
            collected.lock().unwrap().push(progress.written_bytes);
        });
        let observer = FastbootProgressObserver {
            parser: Mutex::new(FastbootProgressParser::new(total)),
            sink,
            context: Arc::new(Mutex::new(FastbootProgressContext {
                partition_name: "system".to_string(),
                total_bytes: total,
                partition_index: 1,
                partition_total: 1,
            })),
        };

        // 第一块传完 64MiB，第二块只开了一半就结束（没有 Writing / Finished 行）。
        let first = b"Sending sparse 'system' (65536 KB)...\nsystem: 65536 KB/65536 KB\n";
        observer
            .observe(ProcessObservation::Output {
                stream: nwflash_windows::process::ProcessOutputStream::Stdout,
                sequence: 0,
                bytes: first,
            })
            .expect("output observation must be accepted");
        let second = b"Sending sparse 'system' (65536 KB)...\nsystem: 32768 KB/65536 KB\n";
        observer
            .observe(ProcessObservation::Output {
                stream: nwflash_windows::process::ProcessOutputStream::Stdout,
                sequence: 1,
                bytes: second,
            })
            .expect("output observation must be accepted");

        let before_finish = *reported.lock().unwrap().last().unwrap();
        assert!(
            before_finish < total,
            "第二块只传了一半，收尾前不该报满：{before_finish} vs {total}"
        );

        observer
            .observe(ProcessObservation::Finished(
                nwflash_windows::process::ProcessFinishMetadata {
                    exit_code: Some(0),
                    termination: nwflash_windows::process::ProcessTermination::Completed,
                    process_tree_termination_requested: false,
                },
            ))
            .expect("finish observation must be accepted");

        assert_eq!(
            *reported.lock().unwrap().last().unwrap(),
            total,
            "命令结束时必须把最后一块的余量补满"
        );
    }

    #[test]
    fn a_cancelled_flash_never_reports_full_progress() {
        // 回归：`finish()` 只看输出里有没有 `FAILED`，而取消/超时触发的终止
        // 通常发生在 fastboot 打出 FAILED 之前。若收尾时无条件补满，一次被用户
        // 打断的刷写会被谎报成 100%——「失败绝不臆造进度」的承诺就失效了。
        let total = 128 * MIB;
        let reported = Arc::new(Mutex::new(Vec::<u64>::new()));
        let collected = Arc::clone(&reported);
        let sink: Arc<SafeFlashPartitionProgressSink> = Arc::new(move |progress| {
            collected.lock().unwrap().push(progress.written_bytes);
        });
        let observer = FastbootProgressObserver {
            parser: Mutex::new(FastbootProgressParser::new(total)),
            sink,
            context: Arc::new(Mutex::new(FastbootProgressContext {
                partition_name: "system".to_string(),
                total_bytes: total,
                partition_index: 1,
                partition_total: 1,
            })),
        };

        observer
            .observe(ProcessObservation::Output {
                stream: nwflash_windows::process::ProcessOutputStream::Stdout,
                sequence: 0,
                bytes: b"Sending 'system' (131072 KB)...\nsystem: 32768 KB/131072 KB\n",
            })
            .expect("output observation must be accepted");

        // 用户取消：进程被终止，退出码不是 0。
        observer
            .observe(ProcessObservation::Finished(
                nwflash_windows::process::ProcessFinishMetadata {
                    exit_code: None,
                    termination:
                        nwflash_windows::process::ProcessTermination::TerminationUnconfirmed,
                    process_tree_termination_requested: true,
                },
            ))
            .expect("finish observation must be accepted");

        let last = *reported.lock().unwrap().last().unwrap();
        assert!(
            last < total,
            "被取消的刷写绝不能补满到 100%：{last} vs {total}"
        );
    }

    struct UselessExecutor;

    impl CancellableProcessExecutor for UselessExecutor {
        fn run(
            &self,
            _spec: ProcessCommand,
            _should_cancel: &mut dyn FnMut() -> bool,
        ) -> Result<ProcessOutput, DomainError> {
            Err(DomainError::Internal("假刷写不该执行进程".to_string()))
        }
    }

    #[test]
    fn a_tiny_simulated_partition_still_reports_progress() {
        // 回归：`simulated_flash::duration` 是整型毫秒截断，小于约 36KB 的镜像算
        // 出来是 0ms，等待循环一次都不执行，因此**一个 tick 都不发**，「当前分区」
        // 进度条在这类分区上完全不动。旧实现把"total 为 0"的分支放在循环体里，
        // 而循环体在 total==0 时根本不会执行——那是个可达性为零的假保护。
        let ticks = Arc::new(Mutex::new(Vec::<u64>::new()));
        let collected = Arc::clone(&ticks);
        let service = SafeFlashExecutionService::new(Arc::new(UselessExecutor));
        let mut canceled = || false;

        let out = service
            .run_simulated_flash(4096, &mut canceled, move |written| {
                collected.lock().unwrap().push(written);
            })
            .expect("假刷写应当成功返回");

        assert_eq!(out.exit_code, 0);
        assert_eq!(
            *ticks.lock().unwrap(),
            vec![4096],
            "0ms 的假刷写也必须上报一次整张镜像已写完"
        );
    }

    #[test]
    fn observer_never_reports_more_than_the_partition_total() {
        // 回归：`report()` 原本按
        // `written_bytes.min(context.total_bytes.max(written_bytes))` 封顶，
        // 而 `.max(written_bytes)` 让内层恒 >= written_bytes，外层于是恒取
        // written_bytes —— 等价于原样放行。解析器的总量来自**本命令镜像**，
        // 这里的总量来自**分区镜像表**，两者不一致时 written 会越过 total，
        // 下游拿到 >100% 的进度。
        let reported = Arc::new(Mutex::new(Vec::<u64>::new()));
        let collected = Arc::clone(&reported);
        let sink: Arc<SafeFlashPartitionProgressSink> = Arc::new(move |progress| {
            collected.lock().unwrap().push(progress.written_bytes);
        });
        // context 认为分区总长 100，但解析器被喂了 200 的镜像。
        let observer = FastbootProgressObserver {
            parser: Mutex::new(FastbootProgressParser::new(200)),
            sink,
            context: Arc::new(Mutex::new(FastbootProgressContext {
                partition_name: "system".to_string(),
                total_bytes: 100,
                partition_index: 1,
                partition_total: 1,
            })),
        };

        observer.report(150);

        assert_eq!(
            *reported.lock().unwrap(),
            vec![100],
            "上报值必须按分区总长封顶，绝不能放行 150"
        );
    }

    #[test]
    fn protocol_failure_lines_are_detected_even_with_zero_exit_code() {
        // 退出码 0 不等于成功：这些协议失败行必须被识别，否则会"半刷后继续刷
        // 下一分区"。`error:` 形态此前只有 `command_detail.rs` 的留痕判定认，
        // 本决策路径漏掉了。
        let failing = |stdout: &str, stderr: &str| ProcessOutput {
            exit_code: 0,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        };

        for output in [
            failing("OKAY", "FAILED (remote: 'write failed')"),
            failing(
                "Sending sparse... (bootloader) REMOTE ERROR: write failure",
                "",
            ),
            failing(
                "Sending 'super' (4096 KB)...",
                "fastboot: error: cannot generate image",
            ),
            failing("", "ERROR: usb_write failed with status e00002be"),
        ] {
            assert!(
                fastboot_output_reports_failure(&output).is_some(),
                "协议失败行必须被识别：{output:?}"
            );
        }

        // 良性输出不得误报。
        for output in [
            failing("OKAY           [  0.005s]", ""),
            failing("Sending 'boot' (4096 KB)...", "Finished. Total time: 1.0s"),
        ] {
            assert!(
                fastboot_output_reports_failure(&output).is_none(),
                "良性输出不得误报：{output:?}"
            );
        }
    }
    #[test]
    fn unrecognised_lines_produce_no_progress() {
        let mut parser = FastbootProgressParser::new(4 * MIB);
        for line in [
            "fastboot: error: cannot load 'nope.img'",
            "",
            "OKAY [  0.001s]",
        ] {
            assert_eq!(parser.observe_line(line), None, "行 {line:?} 不该产生进度");
        }
    }

    #[test]
    fn word_error_matching_does_not_false_positive_on_partition_names() {
        assert!(contains_word_error("fastboot: error: cannot load 'x'"));
        assert!(contains_word_error("FAILED (remote: 'error')"));
        // 分区名里含 error 子串不应当被当成失败。
        assert!(!contains_word_error("Sending 'error_log' (1024 KB)..."));
        assert!(!contains_word_error("error_log: 512 KB/1024 KB"));
    }
}
