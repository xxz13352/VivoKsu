//! Session lifecycle orchestration for heartbeat-driven online session control.

use std::{
    fmt, panic,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::future::BoxFuture;
use nwflash_infrastructure::{
    CloudflareError, HeartbeatAdmission, SecretToken, UpdateRequiredInfo,
};
use nwflash_protection::SessionLease;
use thiserror::Error;
use tokio::{
    sync::{Mutex, RwLock},
    task::JoinHandle,
    time::{sleep, timeout, timeout_at, Instant},
};
use tokio_util::sync::CancellationToken;

pub struct HeartbeatInput {
    pub token: SecretToken,
    pub username: String,
    pub lease: SessionLease,
    pub generation: String,
    pub active: bool,
}

impl fmt::Debug for HeartbeatInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeartbeatInput")
            .field("token", &"[REDACTED]")
            .field("username", &self.username)
            .field("lease", &self.lease)
            .field("generation", &self.generation)
            .field("active", &self.active)
            .finish()
    }
}

pub struct SessionLifecycleSession {
    token: SecretToken,
    username: String,
    lease: SessionLease,
    generation: String,
}

pub struct PreparedSessionLifecycleSession(SessionLifecycleSession);

impl SessionLifecycleSession {
    pub fn new(
        token: SecretToken,
        username: String,
        lease: SessionLease,
        generation: String,
    ) -> Self {
        Self {
            token,
            username,
            lease,
            generation,
        }
    }

    pub fn prepare(
        token: SecretToken,
        username: String,
        lease: SessionLease,
        generation: String,
    ) -> Result<PreparedSessionLifecycleSession, SessionLifecycleError> {
        let session = Self::new(token, username, lease, generation);
        session.validate()?;
        Ok(PreparedSessionLifecycleSession(session))
    }

    fn validate(&self) -> Result<(), SessionLifecycleError> {
        if !self.token.is_header_safe()
            || self.username.trim().is_empty()
            || self.lease.session_id().trim().is_empty()
            || self.generation.trim().is_empty()
        {
            return Err(SessionLifecycleError::Message(
                "signed session input is invalid".to_string(),
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for SessionLifecycleSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionLifecycleSession")
            .field("token", &"[REDACTED]")
            .field("username", &self.username)
            .field("lease", &self.lease)
            .field("generation", &self.generation)
            .finish()
    }
}

pub type HeartbeatCallback = Arc<
    dyn Fn(HeartbeatInput) -> BoxFuture<'static, Result<HeartbeatAdmission, CloudflareError>>
        + Send
        + Sync,
>;
pub type ForceExitCallback = Arc<dyn Fn(String, String) + Send + Sync>;
pub type UpdateRequiredCallback = Arc<dyn Fn(String, UpdateRequiredInfo) + Send + Sync>;
/// 查询是否有任务(刷写/下载等)正在执行:执行期间心跳失败不计数、永不退出。
pub type OperationBusyCheck = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTerminalReason {
    ServerForced,
    SessionUnauthorized,
    SessionConflict,
    UpdateRequired,
    HeartbeatUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIntegrityReason {
    LeaseSignatureInvalid,
    LeaseBindingInvalid,
    LeaseExpired,
    SequenceRollback,
    PinMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTerminalClass {
    Delayed(SessionTerminalReason),
    ImmediateIntegrity(SessionIntegrityReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTerminalDecision {
    pub generation: String,
    pub class: SessionTerminalClass,
}

pub type TerminalDecisionCallback = Arc<dyn Fn(SessionTerminalDecision) + Send + Sync>;

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub const HEARTBEAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const GOODBYE_TIMEOUT: Duration = Duration::from_secs(3);
/// 空闲时连续心跳失败达到该次数即退出软件(任务执行期间不计数;间隔 5s → 约 1 分钟持续离线)。
pub const IDLE_EXIT_FAILURE_THRESHOLD: u8 = 10;
// 瞬时心跳失败不计数下线：与 C# 参考一致，网络抖动只在下个周期静默重试。
pub const SERVER_FORCE_EXIT_MESSAGE: &str = "服务端已终止当前会话。";

#[derive(Debug, Error, Clone)]
pub enum SessionLifecycleError {
    #[error("已在运行中会话")]
    AlreadyRunning,
    #[error("会话未启动")]
    NotStarted,
    #[error("{0}")]
    Message(String),
}

#[derive(Default)]
struct SessionLifecycleState {
    session: RwLock<Option<SessionLifecycleSession>>,
    running_task: Mutex<Option<JoinHandle<()>>>,
    stop_token: Mutex<Option<CancellationToken>>,
    running: AtomicBool,
    healthy: AtomicBool,
    in_callback: AtomicBool,
    transient_failures: AtomicU8,
}

#[derive(Clone)]
pub struct SessionLifecycle {
    heartbeat_fn: HeartbeatCallback,
    on_terminal: Option<TerminalDecisionCallback>,
    on_force_exit: Option<ForceExitCallback>,
    on_update_required: Option<UpdateRequiredCallback>,
    operation_busy_check: Option<OperationBusyCheck>,
    heartbeat_interval: Duration,
    request_timeout: Duration,
    goodbye_timeout: Duration,
    state: Arc<SessionLifecycleState>,
}

impl SessionLifecycle {
    pub fn new(
        heartbeat_fn: HeartbeatCallback,
        on_force_exit: Option<ForceExitCallback>,
        on_update_required: Option<UpdateRequiredCallback>,
    ) -> Self {
        Self::with_intervals(
            heartbeat_fn,
            on_force_exit,
            on_update_required,
            HEARTBEAT_INTERVAL,
            HEARTBEAT_REQUEST_TIMEOUT,
            GOODBYE_TIMEOUT,
        )
    }

    pub fn new_with_terminal(
        heartbeat_fn: HeartbeatCallback,
        on_terminal: Option<TerminalDecisionCallback>,
        on_force_exit: Option<ForceExitCallback>,
        on_update_required: Option<UpdateRequiredCallback>,
        operation_busy_check: Option<OperationBusyCheck>,
    ) -> Self {
        Self::with_intervals_and_terminal(
            heartbeat_fn,
            on_terminal,
            on_force_exit,
            on_update_required,
            operation_busy_check,
            HEARTBEAT_INTERVAL,
            HEARTBEAT_REQUEST_TIMEOUT,
            GOODBYE_TIMEOUT,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_intervals(
        heartbeat_fn: HeartbeatCallback,
        on_force_exit: Option<ForceExitCallback>,
        on_update_required: Option<UpdateRequiredCallback>,
        heartbeat_interval: Duration,
        request_timeout: Duration,
        goodbye_timeout: Duration,
    ) -> Self {
        Self::with_intervals_and_terminal(
            heartbeat_fn,
            None,
            on_force_exit,
            on_update_required,
            None,
            heartbeat_interval,
            request_timeout,
            goodbye_timeout,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_intervals_and_terminal(
        heartbeat_fn: HeartbeatCallback,
        on_terminal: Option<TerminalDecisionCallback>,
        on_force_exit: Option<ForceExitCallback>,
        on_update_required: Option<UpdateRequiredCallback>,
        operation_busy_check: Option<OperationBusyCheck>,
        heartbeat_interval: Duration,
        request_timeout: Duration,
        goodbye_timeout: Duration,
    ) -> Self {
        Self {
            heartbeat_fn,
            on_terminal,
            on_force_exit,
            on_update_required,
            operation_busy_check,
            heartbeat_interval,
            request_timeout,
            goodbye_timeout,
            state: Arc::new(SessionLifecycleState::default()),
        }
    }

    pub async fn is_running(&self) -> bool {
        self.state.running.load(Ordering::Acquire)
    }

    pub fn is_healthy(&self) -> bool {
        self.state.healthy.load(Ordering::Acquire)
    }

    pub async fn session_id(&self) -> Option<String> {
        self.state
            .session
            .read()
            .await
            .as_ref()
            .map(|session| session.lease.session_id().to_string())
    }

    pub async fn generation(&self) -> Option<String> {
        self.state
            .session
            .read()
            .await
            .as_ref()
            .map(|session| session.generation.clone())
    }

    pub async fn start(
        &self,
        session: SessionLifecycleSession,
    ) -> Result<(), SessionLifecycleError> {
        session.validate()?;

        let mut task = self.state.running_task.lock().await;
        if self.state.running.load(Ordering::Acquire) || self.state.session.read().await.is_some() {
            return Err(SessionLifecycleError::AlreadyRunning);
        }

        self.install_prepared(PreparedSessionLifecycleSession(session), &mut task)
            .await;
        Ok(())
    }

    pub async fn start_prepared(&self, prepared: PreparedSessionLifecycleSession) {
        let mut task = self.state.running_task.lock().await;
        assert!(
            !self.state.running.load(Ordering::Acquire)
                && self.state.session.read().await.is_none(),
            "prepared lifecycle activation requires completed teardown"
        );
        self.install_prepared(prepared, &mut task).await;
    }

    async fn install_prepared(
        &self,
        prepared: PreparedSessionLifecycleSession,
        task: &mut Option<JoinHandle<()>>,
    ) {
        *self.state.session.write().await = Some(prepared.0);
        let stop_token = CancellationToken::new();
        *self.state.stop_token.lock().await = Some(stop_token.clone());
        self.state.transient_failures.store(0, Ordering::Release);
        self.state.healthy.store(false, Ordering::Release);
        self.state.running.store(true, Ordering::Release);
        *task = Some(self.spawn_heartbeat_loop(stop_token));
    }

    /// Explicit closeout used by logout/user stop and later by the Task 6 supervisor.
    pub async fn stop(&self) -> Result<(), SessionLifecycleError> {
        self.stop_with_goodbye_timeout(self.goodbye_timeout).await
    }

    pub async fn stop_with_goodbye_timeout(
        &self,
        goodbye_timeout: Duration,
    ) -> Result<(), SessionLifecycleError> {
        if self.state.session.read().await.is_none() {
            return Err(SessionLifecycleError::NotStarted);
        }

        if let Some(stop_token) = self.state.stop_token.lock().await.take() {
            stop_token.cancel();
        }
        if let Some(handle) = self.state.running_task.lock().await.take() {
            if !self.state.in_callback.load(Ordering::Acquire) {
                let _ = handle.await;
            }
        }

        self.send_goodbye_with_timeout(goodbye_timeout).await;
        self.clear_session().await;
        Ok(())
    }

    pub async fn close_for_exit(&self, deadline: Instant) -> Result<(), SessionLifecycleError> {
        self.close_for_exit_with_policy(deadline, true).await
    }

    pub async fn close_for_exit_with_policy(
        &self,
        deadline: Instant,
        send_goodbye: bool,
    ) -> Result<(), SessionLifecycleError> {
        if self.state.session.read().await.is_none() {
            return Err(SessionLifecycleError::NotStarted);
        }

        if let Some(stop_token) = self.state.stop_token.lock().await.take() {
            stop_token.cancel();
        }
        if let Some(mut handle) = self.state.running_task.lock().await.take() {
            let must_abort =
                if self.state.in_callback.load(Ordering::Acquire) || deadline <= Instant::now() {
                    true
                } else {
                    timeout_at(deadline, &mut handle).await.is_err()
                };
            if must_abort {
                handle.abort();
                let _ = handle.await;
            }
        }

        if send_goodbye && deadline > Instant::now() {
            self.send_goodbye_until(deadline).await;
        }
        self.clear_session().await;
        Ok(())
    }

    async fn send_goodbye_with_timeout(&self, goodbye_timeout: Duration) {
        let Some(input) = self.heartbeat_input(false).await else {
            return;
        };
        let call = (self.heartbeat_fn)(input);
        let _ = timeout(goodbye_timeout, call).await;
    }

    async fn send_goodbye_until(&self, deadline: Instant) {
        let Some(input) = self.heartbeat_input(false).await else {
            return;
        };
        let call = (self.heartbeat_fn)(input);
        let _ = timeout_at(deadline, call).await;
    }

    async fn clear_session(&self) {
        if let Some(mut session) = self.state.session.write().await.take() {
            session.token.zeroize();
        }
        self.state.running.store(false, Ordering::Release);
        self.state.healthy.store(false, Ordering::Release);
        self.state.transient_failures.store(0, Ordering::Release);
    }

    async fn heartbeat_input(&self, active: bool) -> Option<HeartbeatInput> {
        self.state
            .session
            .read()
            .await
            .as_ref()
            .map(|session| HeartbeatInput {
                token: session.token.request_scope(),
                username: session.username.clone(),
                lease: session.lease.clone(),
                generation: session.generation.clone(),
                active,
            })
    }

    fn spawn_heartbeat_loop(&self, stop_token: CancellationToken) -> JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                if stop_token.is_cancelled() {
                    break;
                }
                if this.tick().await.should_stop {
                    break;
                }
                if stop_token.is_cancelled() {
                    break;
                }
                tokio::select! {
                    _ = stop_token.cancelled() => break,
                    _ = sleep(this.heartbeat_interval) => {}
                }
            }
            this.state.running.store(false, Ordering::Release);
        })
    }

    async fn tick(&self) -> TickResult {
        let Some(input) = self.heartbeat_input(true).await else {
            return TickResult::stop();
        };
        let previous_session_id = input.lease.session_id().to_string();
        let previous_sequence = input.lease.sequence();
        let generation = input.generation.clone();
        let response = timeout(self.request_timeout, (self.heartbeat_fn)(input)).await;

        match response {
            Ok(Ok(HeartbeatAdmission::Accepted(next))) => {
                if next.session_id() != previous_session_id {
                    return self.terminal_force_exit(
                        generation,
                        SessionTerminalClass::ImmediateIntegrity(
                            SessionIntegrityReason::LeaseBindingInvalid,
                        ),
                        "会话租约绑定校验失败".to_string(),
                    );
                }
                if next.sequence() <= previous_sequence {
                    return self.terminal_force_exit(
                        generation,
                        SessionTerminalClass::ImmediateIntegrity(
                            SessionIntegrityReason::SequenceRollback,
                        ),
                        "会话租约序号校验失败".to_string(),
                    );
                }

                let mut stored = self.state.session.write().await;
                let Some(session) = stored.as_mut() else {
                    return TickResult::stop();
                };
                if session.lease.session_id() != previous_session_id
                    || session.lease.sequence() != previous_sequence
                {
                    return TickResult::stop();
                }
                session.lease = next;
                self.state.transient_failures.store(0, Ordering::Release);
                self.state.healthy.store(true, Ordering::Release);
                TickResult::continue_()
            }
            Ok(Ok(HeartbeatAdmission::ForceExit)) => self.terminal_force_exit(
                generation,
                SessionTerminalClass::Delayed(SessionTerminalReason::ServerForced),
                SERVER_FORCE_EXIT_MESSAGE.to_string(),
            ),
            Ok(Ok(HeartbeatAdmission::Goodbye)) => self.terminal_force_exit(
                generation,
                SessionTerminalClass::Delayed(SessionTerminalReason::ServerForced),
                "活动心跳响应无有效租约".to_string(),
            ),
            Ok(Err(CloudflareError::UpdateRequired(update))) => {
                self.terminal_update(generation, update)
            }
            // 网络传输失败:只标记不健康静默重试,绝不终止会话。
            Ok(Err(CloudflareError::Transport(_))) => {
                self.record_transient_failure(generation)
            }
            // 会话类失败(401/403/409/410 等 ApiError):不再终止会话——任务执行
            // 不得被心跳错误打断,退出客户端的唯一路径是服务端 force_exit。
            // 标记不健康静默重试(服务端要终止时会显式下发 force_exit)。
            Ok(Err(CloudflareError::ApiError { .. })) => {
                self.record_transient_failure(generation)
            }
            // 完整性失败(VMP 签名租约校验)与其它非瞬态错误保留终结语义。
            Ok(Err(error)) => self.terminal_force_exit(
                generation,
                terminal_classification(&error),
                terminal_reason(&error),
            ),
            Err(_) => self.record_transient_failure(generation),
        }
    }

    fn record_transient_failure(&self, generation: String) -> TickResult {
        // 瞬时失败(网络抖动 / 429 / 5xx / 超时)与会话类失败(401/403/409/410)
        // 都只标记不健康并在下个周期静默重试——与 C# HeartbeatService 的兜底语义一致。
        // 例外:任务执行期间绝不退出(刷写/下载不得被打断),失败不计数;空闲时连续
        // IDLE_EXIT_FAILURE_THRESHOLD 次失败则走 force_exit 通道退出软件
        // (服务端在租约到期后也会自行清理,这里兜底客户端已不可能恢复的场景)。
        self.state.healthy.store(false, Ordering::Release);
        let busy = self
            .operation_busy_check
            .as_ref()
            .map(|check| check())
            .unwrap_or(false);
        if busy {
            // 任务执行期间:失败不计数(已有计数也清零——忙完后的空闲窗口重新起算)。
            self.state.transient_failures.store(0, Ordering::Release);
            return TickResult::continue_();
        }

        let failures = self.state.transient_failures.fetch_add(1, Ordering::AcqRel) + 1;
        if failures < IDLE_EXIT_FAILURE_THRESHOLD {
            return TickResult::continue_();
        }

        self.terminal_force_exit(
            generation,
            SessionTerminalClass::Delayed(SessionTerminalReason::ServerForced),
            "连续 10 次心跳失败,软件已退出。".to_string(),
        )
    }

    fn terminal_force_exit(
        &self,
        generation: String,
        class: SessionTerminalClass,
        reason: String,
    ) -> TickResult {
        self.state.healthy.store(false, Ordering::Release);
        self.state.running.store(false, Ordering::Release);
        self.state.in_callback.store(true, Ordering::Release);
        if let Some(callback) = self.on_terminal.as_ref() {
            let decision = SessionTerminalDecision {
                generation: generation.clone(),
                class,
            };
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(decision)));
        }
        if let Some(callback) = self.on_force_exit.as_ref() {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(generation, reason)));
        }
        self.state.in_callback.store(false, Ordering::Release);
        TickResult::stop()
    }

    fn terminal_update(&self, generation: String, update: UpdateRequiredInfo) -> TickResult {
        self.state.healthy.store(false, Ordering::Release);
        self.state.running.store(false, Ordering::Release);
        self.state.in_callback.store(true, Ordering::Release);
        if let Some(callback) = self.on_terminal.as_ref() {
            let decision = SessionTerminalDecision {
                generation: generation.clone(),
                class: SessionTerminalClass::Delayed(SessionTerminalReason::UpdateRequired),
            };
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(decision)));
        }
        if let Some(callback) = self.on_update_required.as_ref() {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(generation, update)));
        }
        self.state.in_callback.store(false, Ordering::Release);
        TickResult::stop()
    }
}

// 会话类 ApiError(401/403/409/410)已在 tick 静默吞掉,到这里的只剩
// 完整性校验失败与 426 强制更新;下方两函数的 ApiError 分支保留作防御分类。

fn terminal_reason(error: &CloudflareError) -> String {
    match error {
        CloudflareError::Integrity(_) => "会话完整性校验失败".to_string(),
        CloudflareError::ApiError {
            status: 401 | 403, ..
        } => "账号已被停用或登录失效".to_string(),
        CloudflareError::ApiError { status: 409, .. } => "会话租约冲突".to_string(),
        _ => "心跳响应被拒绝".to_string(),
    }
}

fn terminal_classification(error: &CloudflareError) -> SessionTerminalClass {
    use nwflash_infrastructure::IntegrityFailure;

    match error {
        CloudflareError::Integrity(
            IntegrityFailure::LeaseSignature | IntegrityFailure::InvalidVerificationKey,
        ) => {
            SessionTerminalClass::ImmediateIntegrity(SessionIntegrityReason::LeaseSignatureInvalid)
        }
        CloudflareError::Integrity(IntegrityFailure::LeaseTime) => {
            SessionTerminalClass::ImmediateIntegrity(SessionIntegrityReason::LeaseExpired)
        }
        CloudflareError::Integrity(IntegrityFailure::LeaseSequence) => {
            SessionTerminalClass::ImmediateIntegrity(SessionIntegrityReason::SequenceRollback)
        }
        CloudflareError::Integrity(
            IntegrityFailure::SpkiMismatch
            | IntegrityFailure::InvalidApiEndpoint
            | IntegrityFailure::InvalidPinset
            | IntegrityFailure::PinsetSignature
            | IntegrityFailure::PinsetHost
            | IntegrityFailure::PinsetTime
            | IntegrityFailure::PinsetRollback
            | IntegrityFailure::PinsetCache
            | IntegrityFailure::PinsetEnvelope
            | IntegrityFailure::TlsConfiguration,
        ) => SessionTerminalClass::ImmediateIntegrity(SessionIntegrityReason::PinMismatch),
        CloudflareError::Integrity(_) => {
            SessionTerminalClass::ImmediateIntegrity(SessionIntegrityReason::LeaseBindingInvalid)
        }
        CloudflareError::ApiError {
            status: 401 | 403, ..
        } => SessionTerminalClass::Delayed(SessionTerminalReason::SessionUnauthorized),
        CloudflareError::ApiError { status: 409, .. } => {
            SessionTerminalClass::Delayed(SessionTerminalReason::SessionConflict)
        }
        CloudflareError::UpdateRequired(_) => {
            SessionTerminalClass::Delayed(SessionTerminalReason::UpdateRequired)
        }
        _ => SessionTerminalClass::Delayed(SessionTerminalReason::ServerForced),
    }
}

struct TickResult {
    should_stop: bool,
}

impl TickResult {
    fn stop() -> Self {
        Self { should_stop: true }
    }

    fn continue_() -> Self {
        Self { should_stop: false }
    }
}
