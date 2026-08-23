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
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

pub struct HeartbeatInput {
    pub token: SecretToken,
    pub username: String,
    pub lease: SessionLease,
    pub active: bool,
}

impl fmt::Debug for HeartbeatInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeartbeatInput")
            .field("token", &"[REDACTED]")
            .field("username", &self.username)
            .field("lease", &self.lease)
            .field("active", &self.active)
            .finish()
    }
}

pub struct SessionLifecycleSession {
    token: SecretToken,
    username: String,
    lease: SessionLease,
}

impl SessionLifecycleSession {
    pub fn new(token: SecretToken, username: String, lease: SessionLease) -> Self {
        Self {
            token,
            username,
            lease,
        }
    }
}

impl fmt::Debug for SessionLifecycleSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionLifecycleSession")
            .field("token", &"[REDACTED]")
            .field("username", &self.username)
            .field("lease", &self.lease)
            .finish()
    }
}

pub type HeartbeatCallback = Arc<
    dyn Fn(HeartbeatInput) -> BoxFuture<'static, Result<HeartbeatAdmission, CloudflareError>>
        + Send
        + Sync,
>;
pub type ForceExitCallback = Arc<dyn Fn(String) + Send + Sync>;
pub type UpdateRequiredCallback = Arc<dyn Fn(UpdateRequiredInfo) + Send + Sync>;

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub const HEARTBEAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const GOODBYE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CONSECUTIVE_TRANSIENT_FAILURES: u8 = 3;

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
    on_force_exit: Option<ForceExitCallback>,
    on_update_required: Option<UpdateRequiredCallback>,
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

    #[allow(clippy::too_many_arguments)]
    pub fn with_intervals(
        heartbeat_fn: HeartbeatCallback,
        on_force_exit: Option<ForceExitCallback>,
        on_update_required: Option<UpdateRequiredCallback>,
        heartbeat_interval: Duration,
        request_timeout: Duration,
        goodbye_timeout: Duration,
    ) -> Self {
        Self {
            heartbeat_fn,
            on_force_exit,
            on_update_required,
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

    pub async fn start(
        &self,
        session: SessionLifecycleSession,
    ) -> Result<(), SessionLifecycleError> {
        if session.token.is_empty()
            || session.username.trim().is_empty()
            || session.lease.session_id().trim().is_empty()
        {
            return Err(SessionLifecycleError::Message(
                "signed session 与 token 不能为空".to_string(),
            ));
        }

        let mut task = self.state.running_task.lock().await;
        if self.state.running.load(Ordering::Acquire) || self.state.session.read().await.is_some() {
            return Err(SessionLifecycleError::AlreadyRunning);
        }

        *self.state.session.write().await = Some(session);
        let stop_token = CancellationToken::new();
        *self.state.stop_token.lock().await = Some(stop_token.clone());
        self.state.transient_failures.store(0, Ordering::Release);
        self.state.healthy.store(false, Ordering::Release);
        self.state.running.store(true, Ordering::Release);
        *task = Some(self.spawn_heartbeat_loop(stop_token));
        Ok(())
    }

    /// Explicit closeout used by logout/user stop and later by the Task 6 supervisor.
    pub async fn stop(&self) -> Result<(), SessionLifecycleError> {
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

        self.send_goodbye().await;
        if let Some(mut session) = self.state.session.write().await.take() {
            session.token.zeroize();
        }
        self.state.running.store(false, Ordering::Release);
        self.state.healthy.store(false, Ordering::Release);
        self.state.transient_failures.store(0, Ordering::Release);
        Ok(())
    }

    async fn send_goodbye(&self) {
        let Some(input) = self.heartbeat_input(false).await else {
            return;
        };
        let call = (self.heartbeat_fn)(input);
        let _ = timeout(self.goodbye_timeout, call).await;
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
        let response = timeout(self.request_timeout, (self.heartbeat_fn)(input)).await;

        match response {
            Ok(Ok(HeartbeatAdmission::Accepted(next))) => {
                if next.session_id() != previous_session_id || next.sequence() <= previous_sequence
                {
                    return self.terminal_force_exit("会话租约序号校验失败".to_string());
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
            Ok(Ok(HeartbeatAdmission::ForceExit(reason))) => self.terminal_force_exit(reason),
            Ok(Ok(HeartbeatAdmission::Goodbye)) => {
                self.terminal_force_exit("活动心跳响应无有效租约".to_string())
            }
            Ok(Err(CloudflareError::UpdateRequired(update))) => self.terminal_update(update),
            Ok(Err(error)) if is_transient(&error) => self.record_transient_failure(),
            Ok(Err(error)) => self.terminal_force_exit(terminal_reason(&error)),
            Err(_) => self.record_transient_failure(),
        }
    }

    fn record_transient_failure(&self) -> TickResult {
        self.state.healthy.store(false, Ordering::Release);
        let failures = self
            .state
            .transient_failures
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if failures == MAX_CONSECUTIVE_TRANSIENT_FAILURES {
            self.terminal_force_exit("连续三次心跳失败".to_string())
        } else {
            TickResult::continue_()
        }
    }

    fn terminal_force_exit(&self, reason: String) -> TickResult {
        self.state.healthy.store(false, Ordering::Release);
        self.state.running.store(false, Ordering::Release);
        self.state.in_callback.store(true, Ordering::Release);
        if let Some(callback) = self.on_force_exit.as_ref() {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(reason)));
        }
        self.state.in_callback.store(false, Ordering::Release);
        TickResult::stop()
    }

    fn terminal_update(&self, update: UpdateRequiredInfo) -> TickResult {
        self.state.healthy.store(false, Ordering::Release);
        self.state.running.store(false, Ordering::Release);
        self.state.in_callback.store(true, Ordering::Release);
        if let Some(callback) = self.on_update_required.as_ref() {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(update)));
        }
        self.state.in_callback.store(false, Ordering::Release);
        TickResult::stop()
    }
}

fn is_transient(error: &CloudflareError) -> bool {
    matches!(error, CloudflareError::Transport(_))
        || matches!(
            error,
            CloudflareError::ApiError { status: 429, .. }
                | CloudflareError::ApiError {
                    status: 500..=599,
                    ..
                }
        )
}

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
