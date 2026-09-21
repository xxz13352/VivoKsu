//! Operation orchestration primitives for NWflash.

use std::{
    cell::Cell,
    collections::BTreeMap,
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use thiserror::Error;
use tokio::{
    sync::{broadcast, Mutex, OwnedSemaphorePermit, RwLock, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use nwflash_domain::{
    DomainError, OperationKind, OperationLogLevel, OperationStateSnapshot, PartitionTaskSnapshot,
    PartitionTaskState, UsageLogDetail, UsageLogEntry,
};

const PROGRESS_THROTTLE: Duration = Duration::from_millis(100);
static OPERATION_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 共享（非设备独占）通道的许可数量。
///
/// 设备独占通道只有 1 个许可：刷写、分区读写/备份/擦除、设备安装与重启等
/// 会争用设备的操作必须串行。其余操作（投屏、固件检查/提取/哈希、驱动安装、
/// 资源校验、ROM 查询）只占用共享通道，彼此不互斥、也不会被设备操作拦截。
///
/// 空闲判定需要同时拿到设备许可与共享通道的**全部**许可，因此该值只影响
/// 共享操作的并发上限，不影响 `try_acquire_idle`/`wait_until_idle` 的语义。
pub const HOST_LANE_PERMITS: u32 = 64;

thread_local! {
    static RUNNING_DISPATCH_ACTIVE: Cell<bool> = const { Cell::new(false) };
    static RUNNING_DISPATCH_VIOLATED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug, Error)]
pub enum OperationCoordinatorError {
    #[error("已有任务正在进行中，请等待其完成或先取消。")]
    InProgress,
    #[error("会话已释放，无法继续操作。")]
    Disposed,
    #[error("应用正在安全退出，无法开始新操作。")]
    ExitPending,
    #[error("应用正在终止，无法开始新操作。")]
    Terminating,
    #[error("操作派发凭据已失效。")]
    StaleDispatchAuthority,
    #[error("同步进程派发异常，协调器已安全终止。")]
    DispatchPanicked,
    #[error("{0}")]
    Denied(String),
    #[error("运行被用户取消。")]
    Canceled,
    #[error("{0}")]
    Failed(String),
}

pub const OPERATION_IN_PROGRESS_MESSAGE: &str = "已有任务正在进行中，请等待其完成或先取消。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationAdmissionState {
    Running,
    ExitPending,
    Terminating,
}

struct AdmissionGateState {
    state: OperationAdmissionState,
    disposed: bool,
    /// 设备独占操作的派发凭据；同一时刻至多一个。
    device_dispatch: Option<ActiveDispatchAuthority>,
    /// 共享（非设备）操作的派发凭据，按 operation_id 索引；可多个并存。
    shared_dispatches: BTreeMap<String, CancellationToken>,
}

struct ActiveDispatchAuthority {
    operation_id: String,
    cancellation: CancellationToken,
}

impl AdmissionGateState {
    /// 是否存在任何正在派发的操作（两条通道的并集）。
    fn has_active_dispatch(&self) -> bool {
        self.device_dispatch.is_some() || !self.shared_dispatches.is_empty()
    }

    fn is_dispatch_valid(&self, operation_id: &str) -> bool {
        if self
            .device_dispatch
            .as_ref()
            .is_some_and(|authority| {
                authority.operation_id == operation_id && !authority.cancellation.is_cancelled()
            })
        {
            return true;
        }
        self.shared_dispatches
            .get(operation_id)
            .is_some_and(|cancellation| !cancellation.is_cancelled())
    }
}

struct AdmissionGate {
    state: StdMutex<AdmissionGateState>,
}

impl AdmissionGate {
    fn lock(&self) -> StdMutexGuard<'_, AdmissionGateState> {
        if RUNNING_DISPATCH_ACTIVE.with(Cell::get) {
            RUNNING_DISPATCH_VIOLATED.with(|violation| violation.set(true));
            panic!("operation coordinator admission reentry during synchronous dispatch");
        }
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.state = OperationAdmissionState::Terminating;
                state.disposed = true;
                state.device_dispatch = None;
                state.shared_dispatches.clear();
                self.state.clear_poison();
                state
            }
        }
    }

    fn with_running_dispatch(
        &self,
        authority: DispatchAuthority<'_>,
        dispatch: impl FnOnce(),
    ) -> Result<(), OperationCoordinatorError> {
        let mut admission = self.lock();
        ensure_running(&admission)?;
        match authority {
            DispatchAuthority::Idle if admission.has_active_dispatch() => {
                return Err(OperationCoordinatorError::InProgress);
            }
            DispatchAuthority::Active(operation_id) => {
                if !admission.is_dispatch_valid(operation_id) {
                    return Err(OperationCoordinatorError::StaleDispatchAuthority);
                }
            }
            DispatchAuthority::Idle => {}
        }

        let outcome = catch_unwind(AssertUnwindSafe(|| {
            let _scope = RunningDispatchScope::enter();
            dispatch();
        }));
        let dispatch_violated =
            RUNNING_DISPATCH_VIOLATED.with(|violation| violation.replace(false));
        if outcome.is_err() || dispatch_violated {
            admission.state = OperationAdmissionState::Terminating;
            admission.disposed = true;
            admission.device_dispatch = None;
            admission.shared_dispatches.clear();
            return Err(OperationCoordinatorError::DispatchPanicked);
        }
        Ok(())
    }

    fn activate_operation(
        self: &Arc<Self>,
        operation_id: String,
        cancellation: &CancellationToken,
        exclusive: bool,
    ) -> Result<ActiveDispatchLease, OperationCoordinatorError> {
        let mut admission = self.lock();
        ensure_running(&admission)?;
        if cancellation.is_cancelled() {
            return Err(OperationCoordinatorError::Canceled);
        }
        if exclusive {
            if admission.device_dispatch.is_some() {
                return Err(OperationCoordinatorError::InProgress);
            }
            admission.device_dispatch = Some(ActiveDispatchAuthority {
                operation_id: operation_id.clone(),
                cancellation: cancellation.clone(),
            });
        } else {
            admission
                .shared_dispatches
                .insert(operation_id.clone(), cancellation.clone());
        }
        Ok(ActiveDispatchLease {
            admission: self.clone(),
            operation_id,
            exclusive,
        })
    }

    fn revoke_and_cancel(&self, operation_id: &str, cancellation: &CancellationToken) {
        let mut admission = self.lock();
        if admission
            .device_dispatch
            .as_ref()
            .is_some_and(|authority| authority.operation_id == operation_id)
        {
            admission.device_dispatch = None;
        }
        admission.shared_dispatches.remove(operation_id);
        cancellation.cancel();
    }
}

enum DispatchAuthority<'a> {
    Idle,
    Active(&'a str),
}

struct RunningDispatchScope;

impl RunningDispatchScope {
    fn enter() -> Self {
        let already_active = RUNNING_DISPATCH_ACTIVE.with(|active| active.replace(true));
        if already_active {
            RUNNING_DISPATCH_VIOLATED.with(|violation| violation.set(true));
            panic!("nested synchronous process dispatch");
        }
        RUNNING_DISPATCH_VIOLATED.with(|violation| violation.set(false));
        Self
    }
}

impl Drop for RunningDispatchScope {
    fn drop(&mut self) {
        RUNNING_DISPATCH_ACTIVE.with(|active| active.set(false));
    }
}

/// Holds exclusive operation admission while teardown requires the coordinator
/// to remain idle. The permit is released when the lease is dropped.
#[must_use = "dropping the idle lease releases operation admission"]
pub struct OperationIdleLease {
    /// 设备独占通道的 1 个许可：证明没有刷写/分区等设备操作在跑。
    _device_permit: OwnedSemaphorePermit,
    /// 共享通道的**全部**许可：证明没有任何共享（非设备）操作在跑。
    _host_barrier: OwnedSemaphorePermit,
    admission: Arc<AdmissionGate>,
}

impl OperationIdleLease {
    /// Runs an eager synchronous dispatch while this lease proves the
    /// operation semaphore is idle.
    ///
    /// The closure must perform the final synchronous side effect and return
    /// `()`. An async block cannot be returned from this API. Do not call back
    /// into the coordinator from the closure.
    ///
    /// ```compile_fail
    /// # use nwflash_application::OperationIdleLease;
    /// fn cannot_defer_dispatch(idle: &OperationIdleLease) {
    ///     let _future = idle
    ///         .with_running_dispatch(|| async { /* deferred spawn */ })
    ///         .unwrap();
    /// }
    /// ```
    pub fn with_running_dispatch(
        &self,
        dispatch: impl FnOnce(),
    ) -> Result<(), OperationCoordinatorError> {
        self.admission
            .with_running_dispatch(DispatchAuthority::Idle, dispatch)
    }
}

struct ActiveDispatchLease {
    admission: Arc<AdmissionGate>,
    operation_id: String,
    exclusive: bool,
}

impl Drop for ActiveDispatchLease {
    fn drop(&mut self) {
        let mut admission = self.admission.lock();
        if self.exclusive {
            if admission
                .device_dispatch
                .as_ref()
                .is_some_and(|authority| authority.operation_id == self.operation_id)
            {
                admission.device_dispatch = None;
            }
        } else {
            admission.shared_dispatches.remove(&self.operation_id);
        }
    }
}

/// 把 `DomainError` 里的具体原因裁剪后写进操作日志区。
///
/// 页面只展示 `public_operation_failure_message` 的通用文案（避免把设备序列号、
/// 本地路径等写进界面），但排障需要看到 adb 的真实 stderr、HTTP 状态码等。这里
/// 只写本地操作日志（不进使用日志上报），并做长度裁剪。
fn operation_failure_detail(error: &DomainError) -> Option<String> {
    let detail = match error {
        // 取消是用户主动行为：没有排障价值，也不该占日志区。
        DomainError::UserCancelled(_) => return None,
        DomainError::DeviceUnavailable(detail)
        | DomainError::AuthorizationDenied(detail)
        | DomainError::RemoteApi(detail)
        | DomainError::ExternalTool(detail)
        | DomainError::InvalidFormat(detail)
        | DomainError::InvalidInput(detail)
        | DomainError::InvalidOperation(detail)
        // 挂起必须留痕:用户需要知道"发生了什么、设备是否安全",这属于
        // 排障信息,不能像取消那样静默。
        | DomainError::WriteSuspended(detail)
        | DomainError::Internal(detail) => detail.trim(),
    };
    if detail.is_empty() {
        return None;
    }
    const MAX_DETAIL_CHARS: usize = 240;
    let sanitized = sanitize_operation_detail(detail);
    if sanitized.is_empty() {
        return None;
    }
    let clipped: String = sanitized.chars().take(MAX_DETAIL_CHARS).collect();
    let ellipsis = if sanitized.chars().count() > MAX_DETAIL_CHARS {
        "…"
    } else {
        ""
    };
    Some(format!("失败详情：{clipped}{ellipsis}"))
}

/// 日志区是给人看的，但也会被抄走/截图：本地路径、URL、`token=` 一类赋值一律隐藏，
/// 只保留"退出码 1：error: no devices"这种真正有用的诊断片段。
const HIDDEN_ASSIGNMENT_KEYS: [&str; 6] = [
    "token",
    "secret",
    "password",
    "passwd",
    "pwd",
    "authorization",
];

/// 供其它命令层复用（例如把服务端错误原因写进日志前先脱敏）。
pub fn sanitize_operation_detail(detail: &str) -> String {
    let mut sanitized = String::with_capacity(detail.len());
    for (index, token) in detail.split_whitespace().enumerate() {
        if index > 0 {
            sanitized.push(' ');
        }
        if hides_failure_token(token) {
            sanitized.push_str("[已隐藏]");
        } else {
            sanitized.push_str(token);
        }
    }
    sanitized
}

fn hides_failure_token(token: &str) -> bool {
    if token.contains("://") {
        return true;
    }
    let mut characters = token.chars();
    let is_windows_path = matches!(characters.next(), Some(first) if first.is_ascii_alphabetic())
        && characters.next() == Some(':')
        && matches!(characters.next(), Some('\\') | Some('/'));
    if is_windows_path {
        return true;
    }
    token.split_once('=').is_some_and(|(key, _)| {
        HIDDEN_ASSIGNMENT_KEYS
            .iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
    })
}

fn public_operation_failure_message(error: &DomainError) -> &'static str {
    match error {
        DomainError::UserCancelled(_) => "操作已取消。",
        // 挂起不是失败、也不是取消:文案必须让用户知道"设备没坏,处理完调试器
        // 就能继续",而不是以为操作已经失败要重刷。
        DomainError::WriteSuspended(_) => {
            "检测到调试器，写入已暂停以确保设备安全。请关闭调试工具后重试，设备未受影响。"
        }
        DomainError::DeviceUnavailable(_) => "设备不可用，请检查连接后重试。",
        DomainError::AuthorizationDenied(_) => "操作授权被拒绝，请重新登录或联系管理员。",
        DomainError::RemoteApi(_) => "服务器暂时不可用，请稍后重试。",
        DomainError::ExternalTool(_) => "外部工具执行失败，请检查设备连接和所需组件后重试。",
        DomainError::InvalidFormat(_) => "所选文件格式无效或不受支持。",
        DomainError::InvalidInput(_) => "操作参数无效，请重新检查后重试。",
        DomainError::InvalidOperation(_) => "当前操作无法完成，请检查设备和所选内容后重试。",
        DomainError::Internal(_) => "操作内部错误，请重试。",
    }
}

#[derive(Debug, Clone)]
pub struct OperationAuthorization {
    pub allowed: bool,
    pub reason: Option<String>,
}

impl OperationAuthorization {
    pub const fn allow() -> Self {
        Self {
            allowed: true,
            reason: None,
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            reason: Some(reason.into()),
        }
    }
}

pub trait OperationPermissionGate: Send + Sync {
    /// `cancellation` 在授权等待期间同样生效（对应 C# 把取消令牌传入
    /// `AuthorizeAsync`）：Stop/取消必须能中断授权等待，而不是等它
    /// 自然返回后再事后补救。
    fn authorize(
        &self,
        operation: OperationKind,
        title: String,
        cancellation: CancellationToken,
    ) -> futures::future::BoxFuture<'static, Result<OperationAuthorization, DomainError>>;
}

pub trait UsageReporter: Send + Sync {
    fn record(&self, entry: UsageLogEntry);
}

pub trait OperationLogger: Send + Sync {
    fn write(&self, level: OperationLogLevel, message: String, operation_id: Option<String>);
}

struct OperationCoordinatorState {
    snapshot: Arc<RwLock<OperationStateSnapshot>>,
    state_changed: broadcast::Sender<OperationStateSnapshot>,
    last_progress_report: Arc<Mutex<Instant>>,
    permission_gate: Option<Arc<dyn OperationPermissionGate>>,
    usage_reporter: Option<Arc<dyn UsageReporter>>,
    logger: Option<Arc<dyn OperationLogger>>,
    /// 各操作的使用日志明细分桶（审计 A10：此前单 Vec + `clear()`，并发
    /// 操作会清掉对方的明细，终态全量 clone 会把多操作明细互相串台）。
    /// 键为 operation_id；收尾时只取走自己的桶，其他操作的明细不受影响。
    operation_details: Arc<StdMutex<std::collections::BTreeMap<String, Vec<UsageLogDetail>>>>,
    notify_blocked: Option<Arc<dyn Fn(String) + Send + Sync>>,
    current_gate: Arc<Mutex<Option<CurrentGate>>>,
}

/// 当前可取消操作的登记项。共享通道下可能有并发操作，收尾时必须按
/// `operation_id` 匹配，避免清掉别的操作的取消凭据。
struct CurrentGate {
    operation_id: String,
    cancellation: CancellationToken,
}

#[derive(Debug, Default)]
struct StageUpdate {
    stage: Option<String>,
    kind: Option<OperationKind>,
    progress: Option<f64>,
    monotonic_progress: bool,
    partition_task: Option<PartitionTaskSnapshot>,
}

#[derive(Clone)]
pub struct OperationContext {
    operation_id: String,
    state: Arc<OperationCoordinatorState>,
    admission: Arc<AdmissionGate>,
}

impl OperationContext {
    pub fn report_stage(&self, stage: impl Into<String>) {
        let stage = stage.into();
        self.state.log(
            OperationLogLevel::Info,
            stage.clone(),
            Some(self.operation_id.clone()),
        );
        self.state.report(StageUpdate {
            stage: Some(stage),
            kind: None,
            progress: None,
            monotonic_progress: false,
            partition_task: None,
        });
    }

    pub fn report_stage_with_kind(&self, stage: impl Into<String>, kind: OperationKind) {
        let stage = stage.into();
        self.state.log(
            OperationLogLevel::Info,
            stage.clone(),
            Some(self.operation_id.clone()),
        );
        self.state.report(StageUpdate {
            stage: Some(stage),
            kind: Some(kind),
            progress: None,
            monotonic_progress: false,
            partition_task: None,
        });
    }

    pub fn report_progress(&self, progress: f64) {
        self.state.report(StageUpdate {
            stage: None,
            kind: None,
            progress: Some(progress),
            monotonic_progress: false,
            partition_task: None,
        });
    }

    pub fn report_progress_monotonic(&self, progress: f64) {
        self.state.report(StageUpdate {
            stage: None,
            kind: None,
            progress: Some(progress),
            monotonic_progress: true,
            partition_task: None,
        });
    }

    pub async fn report_partition_task(
        &self,
        partition_name: impl Into<String>,
        state: PartitionTaskState,
        overall_progress: f64,
    ) {
        let partition_name = partition_name.into();
        self.state.log(
            OperationLogLevel::Info,
            format!("分区 {}：{:?}", partition_name, state),
            Some(self.operation_id.clone()),
        );
        self.state
            .report_now(StageUpdate {
                stage: None,
                kind: None,
                progress: Some(overall_progress),
                monotonic_progress: false,
                partition_task: Some(PartitionTaskSnapshot {
                    partition_name,
                    state,
                    overall_progress: overall_progress.clamp(0.0, 1.0),
                }),
            })
            .await;
    }

    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// 只写一条 Info 明细（本地操作日志 + 使用日志 details），不推进阶段/进度。
    ///
    /// 用于把有价值的结构化结果（例如分区清单）随使用日志一起上报服务器，
    /// 而不会改动界面上的当前阶段文案。
    pub fn report_detail(&self, message: impl Into<String>) {
        let message = message.into();
        self.state.log_with_detail(
            OperationLogLevel::Info,
            message.clone(),
            Some(message),
            Some(self.operation_id.clone()),
        );
    }

    /// 本地操作日志只写 `summary`，上报服务器的使用日志 details 写完整 `detail`。
    ///
    /// 用于「界面要精简、排障要完整」的信息（例如分区名清单）：明细可能上万字符，
    /// 全量写进界面日志会淹没其他记录，但服务器侧仍需要完整名单。
    pub fn report_detail_split(&self, summary: impl Into<String>, detail: impl Into<String>) {
        self.state.log_with_detail(
            OperationLogLevel::Info,
            summary.into(),
            Some(detail.into()),
            Some(self.operation_id.clone()),
        );
    }

    /// 只上报服务器，**不写本地操作日志区**。
    ///
    /// 用于「服务器侧要命令级细节、界面日志必须保持阶段级」的信息。逐条命令的
    /// argv / 退出码 / 输出走这条通道：既让管理端能看到完整执行流水，又不会让
    /// 工具界面的日志区被命令流水淹没。
    pub fn report_usage_detail(&self, detail: impl Into<String>) {
        self.state.log_usage_detail(
            OperationLogLevel::Info,
            detail.into(),
            Some(self.operation_id.clone()),
        );
    }

    /// 把非致命异常写入操作日志（Warning），不推进阶段/进度。用于吞掉
    /// 可容忍失败但仍需留痕的场景，避免把最准确的出错位置静默丢弃。
    pub fn report_warning(&self, message: impl Into<String>) {
        self.state.log(
            OperationLogLevel::Warning,
            message.into(),
            Some(self.operation_id.clone()),
        );
    }

    /// Runs the final eager synchronous dispatch for this active operation.
    ///
    /// The operation ID is checked under the same admission mutex used by the
    /// exit transition. A cloned context becomes unusable as soon as its
    /// `run_async` invocation finishes or is dropped.
    ///
    /// ```compile_fail
    /// # use nwflash_application::OperationContext;
    /// fn cannot_defer_dispatch(context: &OperationContext) {
    ///     let _future = context
    ///         .with_running_dispatch(|| async { /* deferred spawn */ })
    ///         .unwrap();
    /// }
    /// ```
    pub fn with_running_dispatch(
        &self,
        dispatch: impl FnOnce(),
    ) -> Result<(), OperationCoordinatorError> {
        self.admission
            .with_running_dispatch(DispatchAuthority::Active(&self.operation_id), dispatch)
    }
}

#[derive(Clone)]
pub struct OperationCoordinator {
    state: Arc<OperationCoordinatorState>,
    admission: Arc<AdmissionGate>,
    /// 设备独占通道（1 个许可）。
    device_lock: Arc<Semaphore>,
    /// 共享通道（`HOST_LANE_PERMITS` 个许可）：非设备操作之间不互斥。
    host_lane: Arc<Semaphore>,
    _operation_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl Default for OperationCoordinator {
    fn default() -> Self {
        Self::new(None, None, None, None, None)
    }
}

impl OperationCoordinator {
    pub fn new(
        notify_blocked: Option<Arc<dyn Fn(String) + Send + Sync>>,
        permission_gate: Option<Arc<dyn OperationPermissionGate>>,
        usage_reporter: Option<Arc<dyn UsageReporter>>,
        logger: Option<Arc<dyn OperationLogger>>,
        operation_task: Option<JoinHandle<()>>,
    ) -> Self {
        let (state_changed, _) = broadcast::channel(32);
        let state = OperationCoordinatorState {
            snapshot: Arc::new(RwLock::new(OperationStateSnapshot::idle())),
            state_changed,
            last_progress_report: Arc::new(Mutex::new(Instant::now())),
            permission_gate,
            usage_reporter,
            logger,
            operation_details: Arc::new(StdMutex::new(std::collections::BTreeMap::new())),
            notify_blocked,
            current_gate: Arc::new(Mutex::new(None)),
        };

        Self {
            state: Arc::new(state),
            admission: Arc::new(AdmissionGate {
                state: StdMutex::new(AdmissionGateState {
                    state: OperationAdmissionState::Running,
                    disposed: false,
                    device_dispatch: None,
                    shared_dispatches: BTreeMap::new(),
                }),
            }),
            device_lock: Arc::new(Semaphore::new(1)),
            host_lane: Arc::new(Semaphore::new(HOST_LANE_PERMITS as usize)),
            _operation_task: Arc::new(tokio::sync::Mutex::new(operation_task)),
        }
    }

    /// 有任务（设备独占或共享）在协调器里执行中——含授权往返等待期。
    ///
    /// busy 真值由许可通道唯一派生：`run_async`/`run_shared_async` 在进入
    /// 授权往返**之前**就持有对应通道许可（见两函数开头的 acquire），因此
    /// 授权等待期本判定即为真——空闲心跳连败退出不会杀死一个“等待授权
    /// 中的刷写”。共享通道并发操作各自持有许可，任一在跑即忙（先完成的
    /// 操作不再能“替别人”清零 busy）。任务被 abort 时许可由 Drop 归还，
    /// busy 随之复位，不存在卡 true 的路径。
    ///
    /// 注意：host_lane 判定与构造共用 [`HOST_LANE_PERMITS`] 常量——tokio
    /// Semaphore 没有查询最大许可数的 API，若未来按配置构造共享通道，
    /// 必须同步修改此判定（或把容量存入字段）。
    pub fn is_busy(&self) -> bool {
        self.device_lock.available_permits() == 0
            || self.host_lane.available_permits() < HOST_LANE_PERMITS as usize
    }

    pub async fn state(&self) -> OperationStateSnapshot {
        self.state.snapshot.read().await.clone()
    }

    pub fn subscribe_state(&self) -> broadcast::Receiver<OperationStateSnapshot> {
        self.state.state_changed.subscribe()
    }

    pub fn try_acquire_idle(&self) -> Result<OperationIdleLease, OperationCoordinatorError> {
        let device_permit = {
            let admission = self.admission.lock();
            ensure_running(&admission)?;
            self.device_lock
                .clone()
                .try_acquire_owned()
                .map_err(|_| OperationCoordinatorError::InProgress)?
        };
        // 设备许可已拿到；若共享通道仍有操作在跑，`host_barrier` 获取失败时
        // `device_permit` 会在返回前随作用域释放，不会泄漏许可。
        let host_barrier = self
            .host_lane
            .clone()
            .try_acquire_many_owned(HOST_LANE_PERMITS)
            .map_err(|_| OperationCoordinatorError::InProgress)?;

        Ok(OperationIdleLease {
            _device_permit: device_permit,
            _host_barrier: host_barrier,
            admission: self.admission.clone(),
        })
    }

    pub fn admission_state(&self) -> OperationAdmissionState {
        self.admission.lock().state
    }

    pub fn request_exit_pending(&self) -> OperationAdmissionState {
        let mut admission = self.admission.lock();
        if admission.state == OperationAdmissionState::Running {
            admission.state = OperationAdmissionState::ExitPending;
        }
        admission.state
    }

    pub async fn wait_until_idle(&self) -> OperationIdleLease {
        let device_permit = self
            .device_lock
            .clone()
            .acquire_owned()
            .await
            .expect("operation coordinator never closes its device lane");
        let host_barrier = self
            .host_lane
            .clone()
            .acquire_many_owned(HOST_LANE_PERMITS)
            .await
            .expect("operation coordinator never closes its host lane");
        OperationIdleLease {
            _device_permit: device_permit,
            _host_barrier: host_barrier,
            admission: self.admission.clone(),
        }
    }

    pub fn begin_terminating(
        &self,
        _idle: &OperationIdleLease,
    ) -> Result<(), OperationCoordinatorError> {
        let mut admission = self.admission.lock();
        match admission.state {
            OperationAdmissionState::Running => Err(OperationCoordinatorError::InProgress),
            OperationAdmissionState::ExitPending => {
                admission.state = OperationAdmissionState::Terminating;
                Ok(())
            }
            OperationAdmissionState::Terminating => Ok(()),
        }
    }

    async fn finish_denied_admission(
        &self,
        permit: OwnedSemaphorePermit,
        operation_id: &str,
    ) {
        self.state.clear_current(operation_id).await;
        drop(permit);
    }

    /// 运行一个**设备独占**操作：刷写、分区读写/备份/擦除、设备安装/重启、
    /// 文件传输等会争用设备的操作。设备通道只有 1 个许可，因此这类操作之间
    /// 仍然互斥，被占用时返回 [`OperationCoordinatorError::InProgress`]。
    ///
    /// 共享（非设备）操作必须改用 [`Self::run_shared_async`]，否则会被设备
    /// 操作无谓地拦截，也会无谓地拦截设备操作。
    pub async fn run_async<F, Fut>(
        &self,
        kind: OperationKind,
        title: impl Into<String>,
        operation: F,
    ) -> Result<(), OperationCoordinatorError>
    where
        F: FnOnce(OperationContext, CancellationToken) -> Fut + Send,
        Fut: Future<Output = Result<(), DomainError>> + Send,
    {
        let permit = {
            let admission = self.admission.lock();
            ensure_running(&admission)?;
            self.device_lock
                .clone()
                .try_acquire_owned()
                .map_err(|_| OperationCoordinatorError::InProgress)?
        };

        self.run_with_permit(kind, title.into(), permit, true, operation)
            .await
    }

    /// 运行一个**共享（非设备）**操作：投屏、固件检查/提取/哈希、驱动安装、
    /// 资源校验、ROM 查询等。共享通道不与其他共享操作互斥，也不会被设备
    /// 操作拦截，因此这类操作不会收到“已有任务正在进行中”。
    pub async fn run_shared_async<F, Fut>(
        &self,
        kind: OperationKind,
        title: impl Into<String>,
        operation: F,
    ) -> Result<(), OperationCoordinatorError>
    where
        F: FnOnce(OperationContext, CancellationToken) -> Fut + Send,
        Fut: Future<Output = Result<(), DomainError>> + Send,
    {
        let permit = {
            let admission = self.admission.lock();
            ensure_running(&admission)?;
            self.host_lane
                .clone()
                .try_acquire_owned()
                .map_err(|_| OperationCoordinatorError::InProgress)?
        };

        self.run_with_permit(kind, title.into(), permit, false, operation)
            .await
    }

    /// `run_async`/`run_shared_async` 的公共主体：准入许可已经拿到，只做
    /// 授权、快照与终态收尾。`permit` 的释放即归还对应通道的许可。
    /// `exclusive` 决定派发凭据落在设备通道还是共享通道。
    async fn run_with_permit<F, Fut>(
        &self,
        kind: OperationKind,
        title: String,
        permit: OwnedSemaphorePermit,
        exclusive: bool,
        operation: F,
    ) -> Result<(), OperationCoordinatorError>
    where
        F: FnOnce(OperationContext, CancellationToken) -> Fut + Send,
        Fut: Future<Output = Result<(), DomainError>> + Send,
    {
        let cancellation = CancellationToken::new();
        // operation_id 提前生成：授权等待期间就要把取消凭据登记到 current_gate，
        // 而收尾只能清除“自己的”凭据（共享通道下可能有并发的其它操作）。
        let operation_id = build_operation_id();

        {
            let mut current = self.state.current_gate.lock().await;
            *current = Some(CurrentGate {
                operation_id: operation_id.clone(),
                cancellation: cancellation.clone(),
            });
        }

        if let Some(gate) = self.state.permission_gate.as_ref() {
            let authorization = match gate
                .authorize(kind, title.clone(), cancellation.clone())
                .await
            {
                Ok(authorization) => authorization,
                Err(error) => {
                    self.finish_denied_admission(permit, &operation_id).await;
                    return Err(OperationCoordinatorError::Failed(
                        public_operation_failure_message(&error).to_string(),
                    ));
                }
            };

            if !authorization.allowed {
                let reason = authorization
                    .reason
                    .unwrap_or_else(|| "服务端未许可此操作".to_string());
                let message = format!("服务端未许可此操作: {reason}");
                self.state.emit_blocked(message.clone());
                self.state.log(OperationLogLevel::Warning, message, None);
                self.finish_denied_admission(permit, &operation_id).await;
                return Err(OperationCoordinatorError::Denied(reason));
            }
        }

        if cancellation.is_cancelled() {
            self.finish_denied_admission(permit, &operation_id).await;
            return Err(OperationCoordinatorError::Canceled);
        }

        let started_at = epoch_seconds_now();
        let started_at_instant = Instant::now();

        let active_dispatch_lease = match self
            .admission
            .activate_operation(operation_id.clone(), &cancellation, exclusive)
        {
            Ok(lease) => lease,
            Err(error) => {
                self.finish_denied_admission(permit, &operation_id).await;
                return Err(error);
            }
        };
        self.state
            .set_running(&title, kind, operation_id.clone(), true);
        self.state.log(
            OperationLogLevel::Info,
            title.clone(),
            Some(operation_id.clone()),
        );

        let context = OperationContext {
            operation_id: operation_id.clone(),
            state: self.state.clone(),
            admission: self.admission.clone(),
        };

        let outcome = operation(context, cancellation.clone()).await;
        drop(active_dispatch_lease);

        // 终态与空闲快照按顺序落盘（见 set_terminal_then_idle 的说明）。
        let (terminal_snapshot, log_level, log_message, usage_status) = match &outcome {
            Ok(_) => (
                OperationStateSnapshot {
                    kind: OperationKind::Completed,
                    operation_id: Some(operation_id.clone()),
                    title: title.clone(),
                    stage: format!("{title}完成。"),
                    progress: Some(1.0),
                    started_at: Some(started_at),
                    is_cancellable: false,
                    partition_task: None,
                    partition_tasks: Vec::new(),
                },
                OperationLogLevel::Success,
                format!("{title}完成。"),
                "success",
            ),
            Err(DomainError::UserCancelled(_)) => (
                OperationStateSnapshot {
                    kind: OperationKind::Canceled,
                    operation_id: Some(operation_id.clone()),
                    title: title.clone(),
                    stage: format!("{title}已取消。"),
                    progress: Some(0.0),
                    started_at: Some(started_at),
                    is_cancellable: false,
                    partition_task: None,
                    partition_tasks: Vec::new(),
                },
                OperationLogLevel::Warning,
                format!("{title}已取消。"),
                "canceled",
            ),
            Err(error) => (
                OperationStateSnapshot {
                    kind: OperationKind::Failed,
                    operation_id: Some(operation_id.clone()),
                    title: title.clone(),
                    stage: public_operation_failure_message(error).to_string(),
                    progress: Some(0.0),
                    started_at: Some(started_at),
                    is_cancellable: false,
                    partition_task: None,
                    partition_tasks: Vec::new(),
                },
                OperationLogLevel::Error,
                public_operation_failure_message(error).to_string(),
                "failed",
            ),
        };
        let log_operation_id = Some(operation_id.clone());
        self.state
            .log(log_level, log_message, log_operation_id.clone());
        if let Err(error) = &outcome {
            if let Some(detail) = operation_failure_detail(error) {
                self.state
                    .log(OperationLogLevel::Warning, detail, log_operation_id.clone());
            }
        }

        if let Some(usage_reporter) = self.state.usage_reporter.as_ref() {
            let ended_at = epoch_seconds_now();
            let duration_ms = started_at_instant.elapsed().as_millis().try_into().ok();
            usage_reporter.record(UsageLogEntry {
                operation: format!("{kind:?}"),
                title,
                status: usage_status.to_string(),
                event_id: operation_id.clone(),
                started_at,
                ended_at: Some(ended_at),
                duration_ms,
                details: self
                    .state
                    .operation_details
                    .lock()
                    // 只取走自己的分桶（审计 A10）：并发操作各报各的明细，
                    // 互不清空互不串台；取走后删除该桶，不留无主数据。
                    .map(|mut details| details.remove(&operation_id).unwrap_or_default())
                    .unwrap_or_default(),
            });
        }

        self.state.clear_current(&operation_id).await;
        self.state.set_terminal_then_idle(terminal_snapshot).await;
        drop(permit);

        match outcome {
            Ok(_) => Ok(()),
            Err(DomainError::UserCancelled(_)) => Err(OperationCoordinatorError::Canceled),
            Err(error) => Err(OperationCoordinatorError::Failed(
                public_operation_failure_message(&error).to_string(),
            )),
        }
    }

    pub async fn cancel_current(&self) {
        let current = self.state.current_gate.lock().await;
        if let Some(gate) = current.as_ref() {
            self.admission
                .revoke_and_cancel(&gate.operation_id, &gate.cancellation);
        }
    }

    /// 记录一次发生在 `run_async` 之前的失败（设备探测、命令构建等前置检查）。
    ///
    /// 这些失败不经过 `run_async` 的终态日志路径，此前只会把错误串返回给
    /// 页面内联展示，操作日志区完全不可见。这里补写一条 Error 日志；若
    /// 协调器当前空闲，再按“终态→空闲”的顺序广播一个 Failed 快照，让
    /// 操作日志区能实时显示这条失败记录。协调器忙碌时只落日志，避免覆盖
    /// 正在运行操作的快照。
    ///
    /// 空闲判定用 `try_acquire_idle` 原子完成：获取租约的瞬间即证明「设备
    /// 与共享两条通道都空闲」，不存在「is_busy 读到 false 之后、终态写入
    /// 之前」新操作起跑的窗口——租约在手期间任何 `run_async`/
    /// `run_shared_async` 都会以 InProgress 被拒，不会与本失败快照交错。
    pub async fn report_preflight_failure(&self, title: &str, message: &str) {
        let operation_id = build_operation_id();
        self.state.log(
            OperationLogLevel::Error,
            message.to_string(),
            Some(operation_id.clone()),
        );
        let Ok(idle) = self.try_acquire_idle() else {
            return;
        };
        self.state
            .set_terminal_then_idle(OperationStateSnapshot {
                kind: OperationKind::Failed,
                operation_id: Some(operation_id),
                title: title.to_string(),
                stage: message.to_string(),
                progress: Some(0.0),
                started_at: Some(epoch_seconds_now()),
                is_cancellable: false,
                partition_task: None,
                partition_tasks: Vec::new(),
            })
            .await;
        drop(idle);
    }

    pub fn dispose(&self) {
        self.admission.lock().disposed = true;
    }
}

fn ensure_running(admission: &AdmissionGateState) -> Result<(), OperationCoordinatorError> {
    if admission.disposed {
        return Err(OperationCoordinatorError::Disposed);
    }
    match admission.state {
        OperationAdmissionState::Running => Ok(()),
        OperationAdmissionState::ExitPending => Err(OperationCoordinatorError::ExitPending),
        OperationAdmissionState::Terminating => Err(OperationCoordinatorError::Terminating),
    }
}

impl OperationCoordinatorState {
    fn set_running(
        &self,
        title: &str,
        kind: OperationKind,
        operation_id: String,
        is_cancellable: bool,
    ) {
        self.update(OperationStateSnapshot {
            kind,
            operation_id: Some(operation_id),
            title: title.to_string(),
            stage: title.to_string(),
            progress: Some(0.0),
            started_at: Some(epoch_seconds_now()),
            is_cancellable,
            partition_task: None,
            partition_tasks: Vec::new(),
        });
    }

    /// 终态→空闲的顺序化写入：终态与空闲若分别 spawn 在多线程运行时上
    /// 无先后保证，空闲可能先落地而终态滞留，UI 就会看到“已完成但
    /// busy 卡住”的陈旧快照。终态收尾必须等待写入完成后再置空闲。
    async fn set_terminal_then_idle(&self, terminal: OperationStateSnapshot) {
        self.update_internal(terminal).await;
        self.update_internal(OperationStateSnapshot::idle()).await;
    }

    async fn clear_current(&self, operation_id: &str) {
        let mut current = self.current_gate.lock().await;
        if current
            .as_ref()
            .is_some_and(|gate| gate.operation_id == operation_id)
        {
            current.take();
        }
    }

    fn log(&self, level: OperationLogLevel, message: String, operation_id: Option<String>) {
        let detail = message.clone();
        self.log_with_detail(level, message, Some(detail), operation_id);
    }

    /// `local_message` 写本地操作日志（界面可见），`detail` 写上报服务器的使用日志
    /// details（`None` 表示这条不上报）。
    fn log_with_detail(
        &self,
        level: OperationLogLevel,
        local_message: String,
        detail: Option<String>,
        operation_id: Option<String>,
    ) {
        if let Some(detail) = detail {
            self.push_usage_detail(level, detail, operation_id.as_deref());
        }
        if let Some(logger) = self.logger.as_ref() {
            logger.write(level, local_message, operation_id);
        }
    }

    /// 只把明细写进上报服务器的使用日志，**不写本地操作日志区**。
    ///
    /// 用于「服务器侧要完整、界面必须保持原样」的信息（例如逐条命令的 argv /
    /// 退出码 / 输出）。本地日志区只保留阶段级文案，避免被命令流水淹没。
    fn log_usage_detail(
        &self,
        level: OperationLogLevel,
        detail: String,
        operation_id: Option<String>,
    ) {
        self.push_usage_detail(level, detail, operation_id.as_deref());
    }

    /// 明细分桶的唯一写入点：按 `operation_id` 分桶、跳过连续重复项、每桶保留
    /// 最近 500 条、最多保留 16 个桶。
    fn push_usage_detail(
        &self,
        level: OperationLogLevel,
        detail: String,
        operation_id: Option<&str>,
    ) {
        let Some(operation_id) = operation_id else {
            return;
        };
        let Ok(mut details) = self.operation_details.lock() else {
            return;
        };
        let bucket = details.entry(operation_id.to_string()).or_default();
        if bucket
            .last()
            .is_none_or(|entry| entry.level != level || entry.message != detail)
        {
            bucket.push(UsageLogDetail {
                timestamp_utc: epoch_seconds_now(),
                level,
                message: detail,
            });
        }
        if bucket.len() > 500 {
            let drain = bucket.len() - 500;
            bucket.drain(0..drain);
        }
        // 分桶总上限：防止长期运行会话积累无界内存（每个桶
        // 已限 500 条，桶数按最近操作限制）。
        while details.len() > 16 {
            let oldest = details.keys().next().cloned();
            match oldest {
                Some(key) => {
                    details.remove(&key);
                }
                None => break,
            }
        }
    }

    fn emit_blocked(&self, message: String) {
        if let Some(callback) = self.notify_blocked.as_ref() {
            callback(message);
        }
    }

    fn update(&self, snapshot: OperationStateSnapshot) {
        let this = self.clone();
        tokio::spawn(async move {
            this.update_internal(snapshot).await;
        });
    }

    async fn update_internal(&self, mut snapshot: OperationStateSnapshot) {
        let mut guard = self.snapshot.write().await;
        if snapshot.operation_id.is_none() {
            snapshot.operation_id = guard.operation_id.clone();
        }

        *guard = snapshot.clone();
        drop(guard);
        let _ = self.state_changed.send(snapshot);
    }

    fn report(&self, update: StageUpdate) {
        let this = self.clone();
        tokio::spawn(async move {
            this.report_now(update).await;
        });
    }

    async fn report_now(&self, update: StageUpdate) {
        let mut emit = false;

        let mut current = self.snapshot.write().await;

        if let Some(stage) = update.stage {
            if stage != current.stage {
                emit = true;
            }
            current.stage = stage;
        }

        if let Some(kind) = update.kind {
            if kind != current.kind {
                emit = true;
            }
            current.kind = kind;
        }

        if let Some(progress) = update.progress {
            let next_progress = progress.clamp(0.0, 1.0);
            if update.monotonic_progress
                && current.progress.is_some_and(|value| next_progress <= value)
            {
                return;
            }
            if current.progress != Some(next_progress) {
                let now = Instant::now();
                let mut last = self.last_progress_report.lock().await;
                let can_emit_progress = now.duration_since(*last) >= PROGRESS_THROTTLE;
                if update.monotonic_progress {
                    current.progress = Some(next_progress);
                }
                if emit || can_emit_progress {
                    emit = true;
                    *last = now;
                    current.progress = Some(next_progress);
                }
            }
        }

        if let Some(partition_task) = update.partition_task {
            let existing = current
                .partition_tasks
                .iter_mut()
                .find(|task| task.partition_name == partition_task.partition_name);
            let changed = match existing {
                Some(task) if task == &partition_task => false,
                Some(task) => {
                    *task = partition_task.clone();
                    true
                }
                None => {
                    current.partition_tasks.push(partition_task.clone());
                    true
                }
            };
            if changed || current.partition_task.as_ref() != Some(&partition_task) {
                emit = true;
                current.partition_task = Some(partition_task);
            }
        }

        if emit {
            let snapshot = current.clone();
            drop(current);
            let _ = self.state_changed.send(snapshot);
        }
    }
}

impl Clone for OperationCoordinatorState {
    fn clone(&self) -> Self {
        Self {
            snapshot: self.snapshot.clone(),
            state_changed: self.state_changed.clone(),
            last_progress_report: self.last_progress_report.clone(),
            permission_gate: self.permission_gate.clone(),
            usage_reporter: self.usage_reporter.clone(),
            logger: self.logger.clone(),
            operation_details: self.operation_details.clone(),
            notify_blocked: self.notify_blocked.clone(),
            current_gate: self.current_gate.clone(),
        }
    }
}

fn build_operation_id() -> String {
    let sequence = OPERATION_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", epoch_millis_now(), sequence)
}

fn epoch_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn epoch_millis_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

pub fn operation_id_for_tests() -> String {
    build_operation_id()
}

pub fn result_to_domain_error(error: OperationCoordinatorError) -> DomainError {
    match error {
        OperationCoordinatorError::InProgress => {
            DomainError::InvalidOperation(OPERATION_IN_PROGRESS_MESSAGE.to_string())
        }
        OperationCoordinatorError::Disposed => {
            DomainError::InvalidOperation("会话已释放，无法继续操作。".to_string())
        }
        OperationCoordinatorError::ExitPending => {
            DomainError::InvalidOperation("应用正在安全退出，无法开始新操作。".to_string())
        }
        OperationCoordinatorError::Terminating => {
            DomainError::InvalidOperation("应用正在终止，无法开始新操作。".to_string())
        }
        OperationCoordinatorError::StaleDispatchAuthority => {
            DomainError::InvalidOperation("操作派发凭据已失效。".to_string())
        }
        OperationCoordinatorError::DispatchPanicked => {
            DomainError::Internal("同步进程派发异常，协调器已安全终止。".to_string())
        }
        OperationCoordinatorError::Denied(message) => DomainError::AuthorizationDenied(message),
        OperationCoordinatorError::Canceled => {
            DomainError::UserCancelled("运行被用户取消".to_string())
        }
        OperationCoordinatorError::Failed(message) => DomainError::Internal(message),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::future::BoxFuture;

    use super::*;

    struct FailingPermissionGate;

    impl OperationPermissionGate for FailingPermissionGate {
        fn authorize(
            &self,
            _operation: OperationKind,
            _title: String,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
            Box::pin(async {
                Err(DomainError::RemoteApi(
                    "authorization unavailable".to_string(),
                ))
            })
        }
    }

    #[tokio::test]
    async fn authorization_error_clears_current_gate() {
        let coordinator = OperationCoordinator::new(
            None,
            Some(Arc::new(FailingPermissionGate)),
            None,
            None,
            None,
        );

        let result = coordinator
            .run_async(
                OperationKind::Flashing,
                "authorization error",
                |_, _| async { Ok(()) },
            )
            .await;

        assert!(matches!(result, Err(OperationCoordinatorError::Failed(_))));
        assert!(coordinator.state.current_gate.lock().await.is_none());
    }

    #[test]
    fn poisoned_admission_gate_fails_closed_and_revokes_active_authority() {
        let coordinator = OperationCoordinator::default();
        let admission = coordinator.admission.clone();

        let poison = std::thread::spawn(move || {
            let mut state = admission.state.lock().unwrap();
            state.device_dispatch = Some(ActiveDispatchAuthority {
                operation_id: "poisoned-operation".to_string(),
                cancellation: CancellationToken::new(),
            });
            panic!("synthetic admission gate poison");
        })
        .join();
        assert!(
            poison.is_err(),
            "test thread should poison the admission gate"
        );

        assert_eq!(
            coordinator.admission_state(),
            OperationAdmissionState::Terminating
        );
        let recovered = coordinator
            .admission
            .state
            .lock()
            .expect("fail-closed recovery should clear mutex poison");
        assert!(recovered.disposed);
        assert!(recovered.device_dispatch.is_none());
        assert!(recovered.shared_dispatches.is_empty());
        drop(recovered);
        assert!(matches!(
            coordinator.try_acquire_idle(),
            Err(OperationCoordinatorError::Disposed)
        ));
    }

    #[test]
    fn canceled_token_cannot_install_active_dispatch_authority() {
        let coordinator = OperationCoordinator::default();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let activation = coordinator.admission.activate_operation(
            "canceled-operation".to_string(),
            &cancellation,
            true,
        );

        assert!(matches!(
            activation,
            Err(OperationCoordinatorError::Canceled)
        ));
        assert!(coordinator
            .admission
            .state
            .lock()
            .unwrap()
            .device_dispatch
            .is_none());
    }

    /// 授权门挂起期间（许可已持有、is_busy 尚未置位的旧缺陷窗口），
    /// busy 判定必须已经为真——空闲心跳连败退出不能在这个窗口里
    /// 杀死一个“等待授权中的操作”。
    #[tokio::test]
    async fn busy_is_true_while_authorization_is_pending() {
        // 授权门挂起期间（许可已持有、操作体尚未启动的窗口），busy 判定
        // 必须已经为真——空闲心跳连败退出不能在这个窗口里杀死一个
        // “等待授权中的操作”（P0-1 回归测试）。
        struct PendingGate {
            release: Arc<tokio::sync::Notify>,
        }

        impl OperationPermissionGate for PendingGate {
            fn authorize(
                &self,
                _operation: OperationKind,
                _title: String,
                _cancellation: CancellationToken,
            ) -> BoxFuture<'static, Result<OperationAuthorization, DomainError>> {
                let release = self.release.clone();
                Box::pin(async move {
                    release.notified().await;
                    Ok(OperationAuthorization::allow())
                })
            }
        }

        let release = Arc::new(tokio::sync::Notify::new());
        let gate = PendingGate {
            release: release.clone(),
        };
        let busy_probe = {
            // is_busy 是协调器方法,run_async 拿走 coordinator 后需要
            // 一个克隆来探测;coordinator 是 Clone,克隆放行后再 spawn。
            let coordinator = OperationCoordinator::new(
                None,
                Some(Arc::new(gate)),
                None,
                None,
                None,
            );
            let busy_probe = coordinator.clone();
            tokio::spawn(async move {
                coordinator
                    .run_async(
                        OperationKind::Flashing,
                        "授权中即忙",
                        |_, _| async { Ok(()) },
                    )
                    .await
            });
            busy_probe
        };

        // 等待 run_async 进入授权等待（许可已持有、authorize 未返回）。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            busy_probe.is_busy(),
            "busy must be true while the operation is awaiting authorization"
        );

        release.notify_waiters();
    }

    /// 共享通道上多个操作并发时，任一在跑 busy 即真——先完成的操作
    /// 不能“替别人”清零 busy（P0-2 回归测试）。
    #[tokio::test]
    async fn shared_operations_completing_do_not_clear_busy_for_concurrent_peers() {
        let coordinator = OperationCoordinator::default();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());

        let short = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move {
                coordinator
                    .run_shared_async(OperationKind::Hashing, "short", |_, _| async { Ok(()) })
                    .await
            })
        };

        let long = {
            let coordinator = coordinator.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                coordinator
                    .run_shared_async(OperationKind::Mirroring, "long", move |_, _| {
                        let entered = entered.clone();
                        let release = release.clone();
                        async move {
                            entered.notify_one();
                            release.notified().await;
                            Ok(())
                        }
                    })
                    .await
            })
        };

        // long 的操作体已进入运行态（许可已持有）。
        entered.notified().await;
        // short 完成后，busy 必须因 long 仍在跑而保持为真。
        let short_result = short.await.expect("short shared op should finish");
        assert!(short_result.is_ok());
        assert!(
            coordinator.is_busy(),
            "busy must stay true while a concurrent shared op is still running"
        );

        release.notify_waiters();
        let long_result = long.await.expect("long shared op should finish");
        assert!(long_result.is_ok());
        assert!(!coordinator.is_busy());
    }

    /// run_async 的任务被 abort 后，busy 必须随许可 Drop 复位——不能
    /// 卡 true 导致“心跳空闲退出永不触发”（P1-1 回归测试）。
    #[tokio::test]
    async fn busy_resets_after_operation_task_is_aborted() {
        let coordinator = OperationCoordinator::default();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());

        let handle = {
            let coordinator = coordinator.clone();
            let entered = entered.clone();
            let release = release.clone();
            tokio::spawn(async move {
                coordinator
                    .run_async(OperationKind::Flashing, "aborted", move |_, _| {
                        let entered = entered.clone();
                        let release = release.clone();
                        async move {
                            entered.notify_one();
                            release.notified().await;
                            Ok(())
                        }
                    })
                    .await
            })
        };

        entered.notified().await;
        assert!(coordinator.is_busy());

        handle.abort();
        // abort 触发的 Drop 在调度器上异步执行，轮询等待许可归还。
        let mut reset = false;
        for _ in 0..100 {
            if !coordinator.is_busy() {
                reset = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            reset,
            "busy must reset once the aborted task's permits are dropped"
        );
    }

    /// 终态快照的 started_at 必须是操作真实起点，而不是 None（成功）
    /// 或结束时刻（取消/失败）（P1-3 回归测试）。
    #[tokio::test]
    async fn terminal_snapshots_keep_the_operation_start_time() {
        let coordinator = OperationCoordinator::default();
        let mut receiver = coordinator.subscribe_state();

        let before = epoch_seconds_now();
        coordinator
            .run_async(OperationKind::Flashing, "completed", |_, _| async { Ok(()) })
            .await
            .expect("run should succeed");
        coordinator
            .run_async(OperationKind::Flashing, "failed", |_, _| async {
                Err(DomainError::RemoteApi("boom".to_string()))
            })
            .await
            .expect_err("run should fail");
        let after = epoch_seconds_now();

        // 两段 run 的终态快照都走 await 直写，run 返回时必然已入队；
        // drain 全部消息，检查每个终态快照都保留起点时间。
        let mut saw_completed = false;
        let mut saw_failed = false;
        while let Ok(snapshot) = receiver.try_recv() {
            match snapshot.kind {
                OperationKind::Completed | OperationKind::Failed => {
                    let started_at = snapshot
                        .started_at
                        .expect("terminal snapshot must keep the operation start time");
                    assert!(
                        started_at >= before && started_at <= after,
                        "terminal started_at must be the operation start, not the end: \
                         {started_at} not in [{before}, {after}]"
                    );
                    if snapshot.kind == OperationKind::Completed {
                        saw_completed = true;
                    } else {
                        saw_failed = true;
                    }
                }
                _ => {}
            }
        }
        assert!(
            saw_completed,
            "should observe the Completed terminal snapshot"
        );
        assert!(saw_failed, "should observe the Failed terminal snapshot");
    }

    /// Canceled 终态快照同样必须保留起点时间（对抗审查指出的未测分支）。
    #[tokio::test]
    async fn canceled_snapshot_keeps_the_operation_start_time() {
        let coordinator = OperationCoordinator::default();
        let mut receiver = coordinator.subscribe_state();

        coordinator
            .run_async(OperationKind::Flashing, "cancel-me", |_, token| async move {
                token.cancel();
                Err(DomainError::UserCancelled("用户取消".to_string()))
            })
            .await
            .expect_err("run should be canceled");

        let canceled = loop {
            let snapshot = receiver
                .try_recv()
                .expect("snapshots should be queued after a canceled run");
            if snapshot.kind == OperationKind::Canceled {
                break snapshot;
            }
        };
        assert!(
            canceled.started_at.is_some(),
            "canceled terminal snapshot must keep the operation start time"
        );
    }

    #[tokio::test]
    async fn shared_lane_never_blocks_the_device_lane_but_still_gates_idle() {
        let coordinator = OperationCoordinator::default();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());

        let entered_for_shared = entered.clone();
        let release_for_shared = release.clone();
        let shared = tokio::spawn({
            let coordinator = coordinator.clone();
            async move {
                coordinator
                    .run_shared_async(OperationKind::Mirroring, "shared", move |_, _| {
                        let entered = entered_for_shared.clone();
                        let release = release_for_shared.clone();
                        async move {
                            entered.notify_one();
                            release.notified().await;
                            Ok(())
                        }
                    })
                    .await
            }
        });
        entered.notified().await;

        // 共享操作在跑时，设备独占操作依然能拿到准入，不再返回 InProgress。
        let device = coordinator
            .run_async(OperationKind::Flashing, "device", |_, _| async { Ok(()) })
            .await;
        assert!(
            device.is_ok(),
            "a shared operation must not block the device lane: {device:?}"
        );
        // 但“空闲”依然要求两条通道都清空。
        assert!(coordinator.try_acquire_idle().is_err());

        release.notify_one();
        shared
            .await
            .expect("shared task should join")
            .expect("shared operation should succeed");
        assert!(coordinator.try_acquire_idle().is_ok());
    }

    #[tokio::test]
    async fn shared_operations_overlap_each_other() {
        let coordinator = OperationCoordinator::default();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());

        let entered_for_first = entered.clone();
        let release_for_first = release.clone();
        let first = tokio::spawn({
            let coordinator = coordinator.clone();
            async move {
                coordinator
                    .run_shared_async(OperationKind::Hashing, "first", move |_, _| {
                        let entered = entered_for_first.clone();
                        let release = release_for_first.clone();
                        async move {
                            entered.notify_one();
                            release.notified().await;
                            Ok(())
                        }
                    })
                    .await
            }
        });
        entered.notified().await;

        // 第二个共享操作不会被第一个拦截。
        let second = coordinator
            .run_shared_async(OperationKind::Installing, "second", |_, _| async { Ok(()) })
            .await;
        assert!(
            second.is_ok(),
            "shared operations must not exclude each other: {second:?}"
        );

        release.notify_one();
        first
            .await
            .expect("first task should join")
            .expect("first shared operation should succeed");
    }
}
