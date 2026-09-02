//! Operation orchestration primitives for NWflash.

use std::{
    cell::Cell,
    future::Future,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    active_dispatch: Option<ActiveDispatchAuthority>,
}

struct ActiveDispatchAuthority {
    operation_id: String,
    cancellation: CancellationToken,
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
                state.active_dispatch = None;
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
            DispatchAuthority::Idle if admission.active_dispatch.is_some() => {
                return Err(OperationCoordinatorError::InProgress);
            }
            DispatchAuthority::Active(operation_id) => {
                let Some(active_dispatch) = admission.active_dispatch.as_ref() else {
                    return Err(OperationCoordinatorError::StaleDispatchAuthority);
                };
                if active_dispatch.operation_id != operation_id
                    || active_dispatch.cancellation.is_cancelled()
                {
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
            admission.active_dispatch = None;
            return Err(OperationCoordinatorError::DispatchPanicked);
        }
        Ok(())
    }

    fn activate_operation(
        self: &Arc<Self>,
        operation_id: String,
        cancellation: &CancellationToken,
    ) -> Result<ActiveDispatchLease, OperationCoordinatorError> {
        let mut admission = self.lock();
        ensure_running(&admission)?;
        if cancellation.is_cancelled() {
            return Err(OperationCoordinatorError::Canceled);
        }
        if admission.active_dispatch.is_some() {
            return Err(OperationCoordinatorError::InProgress);
        }

        admission.active_dispatch = Some(ActiveDispatchAuthority {
            operation_id: operation_id.clone(),
            cancellation: cancellation.clone(),
        });
        Ok(ActiveDispatchLease {
            admission: self.clone(),
            operation_id,
        })
    }

    fn revoke_and_cancel(&self, cancellation: &CancellationToken) {
        let mut admission = self.lock();
        admission.active_dispatch = None;
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
    _permit: OwnedSemaphorePermit,
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
}

impl Drop for ActiveDispatchLease {
    fn drop(&mut self) {
        let mut admission = self.admission.lock();
        if admission
            .active_dispatch
            .as_ref()
            .is_some_and(|authority| authority.operation_id == self.operation_id)
        {
            admission.active_dispatch = None;
        }
    }
}

fn public_operation_failure_message(error: &DomainError) -> &'static str {
    match error {
        DomainError::UserCancelled(_) => "操作已取消。",
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
    fn authorize(
        &self,
        operation: OperationKind,
        title: String,
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
    operation_details: Arc<StdMutex<Vec<UsageLogDetail>>>,
    notify_blocked: Option<Arc<dyn Fn(String) + Send + Sync>>,
    current_gate: Arc<Mutex<Option<CancellationToken>>>,
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
    semaphore: Arc<Semaphore>,
    _operation_task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    is_busy: Arc<AtomicBool>,
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
            operation_details: Arc::new(StdMutex::new(Vec::new())),
            notify_blocked,
            current_gate: Arc::new(Mutex::new(None)),
        };

        Self {
            state: Arc::new(state),
            admission: Arc::new(AdmissionGate {
                state: StdMutex::new(AdmissionGateState {
                    state: OperationAdmissionState::Running,
                    disposed: false,
                    active_dispatch: None,
                }),
            }),
            semaphore: Arc::new(Semaphore::new(1)),
            _operation_task: Arc::new(tokio::sync::Mutex::new(operation_task)),
            is_busy: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.is_busy.load(Ordering::Acquire)
    }

    pub async fn state(&self) -> OperationStateSnapshot {
        self.state.snapshot.read().await.clone()
    }

    pub fn subscribe_state(&self) -> broadcast::Receiver<OperationStateSnapshot> {
        self.state.state_changed.subscribe()
    }

    pub fn try_acquire_idle(&self) -> Result<OperationIdleLease, OperationCoordinatorError> {
        let permit = {
            let admission = self.admission.lock();
            ensure_running(&admission)?;
            self.semaphore
                .clone()
                .try_acquire_owned()
                .map_err(|_| OperationCoordinatorError::InProgress)?
        };

        Ok(OperationIdleLease {
            _permit: permit,
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
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("operation coordinator never closes its idle semaphore");
        OperationIdleLease {
            _permit: permit,
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

    async fn finish_denied_admission(&self, permit: OwnedSemaphorePermit) {
        self.state.clear_current().await;
        drop(permit);
    }

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
            self.semaphore
                .clone()
                .try_acquire_owned()
                .map_err(|_| OperationCoordinatorError::InProgress)?
        };

        let title = title.into();
        let cancellation = CancellationToken::new();

        {
            let mut current = self.state.current_gate.lock().await;
            *current = Some(cancellation.clone());
        }

        if let Some(gate) = self.state.permission_gate.as_ref() {
            let authorization = match gate.authorize(kind, title.clone()).await {
                Ok(authorization) => authorization,
                Err(error) => {
                    self.finish_denied_admission(permit).await;
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
                self.finish_denied_admission(permit).await;
                return Err(OperationCoordinatorError::Denied(reason));
            }
        }

        if cancellation.is_cancelled() {
            self.finish_denied_admission(permit).await;
            return Err(OperationCoordinatorError::Canceled);
        }

        let operation_id = build_operation_id();
        let started_at = epoch_seconds_now();
        let started_at_instant = Instant::now();

        let active_dispatch_lease = match self
            .admission
            .activate_operation(operation_id.clone(), &cancellation)
        {
            Ok(lease) => lease,
            Err(error) => {
                self.finish_denied_admission(permit).await;
                return Err(error);
            }
        };
        self.is_busy.store(true, Ordering::Release);
        self.state
            .set_running(&title, kind, operation_id.clone(), true);
        if let Ok(mut details) = self.state.operation_details.lock() {
            details.clear();
        }
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

        let usage_status = match &outcome {
            Ok(_) => {
                self.state
                    .set_completed(operation_id.clone(), &title, &format!("{title}完成。"));
                self.state.log(
                    OperationLogLevel::Success,
                    format!("{title}完成。"),
                    Some(operation_id.clone()),
                );
                Some("success")
            }
            Err(DomainError::UserCancelled(_)) => {
                self.state
                    .set_canceled(operation_id.clone(), &title, &format!("{title}已取消。"));
                self.state.log(
                    OperationLogLevel::Warning,
                    format!("{title}已取消。"),
                    Some(operation_id.clone()),
                );
                Some("canceled")
            }
            Err(error) => {
                let message = public_operation_failure_message(error).to_string();
                self.state
                    .set_failed(operation_id.clone(), &title, &message);
                self.state.log(
                    OperationLogLevel::Error,
                    message,
                    Some(operation_id.clone()),
                );
                Some("failed")
            }
        };

        if let Some(status) = usage_status {
            if let Some(usage_reporter) = self.state.usage_reporter.as_ref() {
                let ended_at = epoch_seconds_now();
                let duration_ms = started_at_instant.elapsed().as_millis().try_into().ok();
                usage_reporter.record(UsageLogEntry {
                    operation: format!("{kind:?}"),
                    title,
                    status: status.to_string(),
                    event_id: operation_id,
                    started_at,
                    ended_at: Some(ended_at),
                    duration_ms,
                    details: self
                        .state
                        .operation_details
                        .lock()
                        .map(|details| details.clone())
                        .unwrap_or_default(),
                });
            }
        }

        self.state.clear_current().await;
        self.state.set_idle();
        self.is_busy.store(false, Ordering::Release);
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
        if let Some(token) = current.as_ref() {
            self.admission.revoke_and_cancel(token);
        }
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

    fn set_completed(&self, operation_id: String, title: &str, stage: &str) {
        self.update(OperationStateSnapshot {
            kind: OperationKind::Completed,
            operation_id: Some(operation_id),
            title: title.to_string(),
            stage: stage.to_string(),
            progress: Some(1.0),
            started_at: None,
            is_cancellable: false,
            partition_task: None,
            partition_tasks: Vec::new(),
        });
    }

    fn set_failed(&self, operation_id: String, title: &str, stage: &str) {
        self.update(OperationStateSnapshot {
            kind: OperationKind::Failed,
            operation_id: Some(operation_id),
            title: title.to_string(),
            stage: stage.to_string(),
            progress: Some(0.0),
            started_at: Some(epoch_seconds_now()),
            is_cancellable: false,
            partition_task: None,
            partition_tasks: Vec::new(),
        });
    }

    fn set_canceled(&self, operation_id: String, title: &str, stage: &str) {
        self.update(OperationStateSnapshot {
            kind: OperationKind::Canceled,
            operation_id: Some(operation_id),
            title: title.to_string(),
            stage: stage.to_string(),
            progress: Some(0.0),
            started_at: Some(epoch_seconds_now()),
            is_cancellable: false,
            partition_task: None,
            partition_tasks: Vec::new(),
        });
    }

    fn set_idle(&self) {
        self.update(OperationStateSnapshot::idle());
    }

    async fn clear_current(&self) {
        let mut current = self.current_gate.lock().await;
        current.take();
    }

    fn log(&self, level: OperationLogLevel, message: String, operation_id: Option<String>) {
        if operation_id.is_some() {
            if let Ok(mut details) = self.operation_details.lock() {
                if details.last().map_or(true, |entry| {
                    entry.level != level || entry.message != message
                }) {
                    details.push(UsageLogDetail {
                        timestamp_utc: epoch_seconds_now(),
                        level,
                        message: message.clone(),
                    });
                }
                if details.len() > 500 {
                    let drain = details.len() - 500;
                    details.drain(0..drain);
                }
            }
        }
        if let Some(logger) = self.logger.as_ref() {
            logger.write(level, message, operation_id);
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
            state.active_dispatch = Some(ActiveDispatchAuthority {
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
        assert!(recovered.active_dispatch.is_none());
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

        let activation = coordinator
            .admission
            .activate_operation("canceled-operation".to_string(), &cancellation);

        assert!(matches!(
            activation,
            Err(OperationCoordinatorError::Canceled)
        ));
        assert!(coordinator
            .admission
            .state
            .lock()
            .unwrap()
            .active_dispatch
            .is_none());
    }
}
