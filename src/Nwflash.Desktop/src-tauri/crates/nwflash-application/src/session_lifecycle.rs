//! Session lifecycle orchestration for heartbeat-driven online session control.

use std::{
    panic,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::future::BoxFuture;
use thiserror::Error;
use tokio::{
    sync::{Mutex, RwLock},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;

use nwflash_infrastructure::api_client::{CloudflareError, HeartbeatResult, UpdateRequiredInfo};

pub type HeartbeatCallback = Arc<
    dyn Fn(String, String, bool) -> BoxFuture<'static, Result<HeartbeatResult, CloudflareError>>
        + Send
        + Sync,
>;
pub type ForceExitCallback = Arc<dyn Fn(String) + Send + Sync>;
pub type UpdateRequiredCallback = Arc<dyn Fn(UpdateRequiredInfo) + Send + Sync>;

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub const HEARTBEAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const GOODBYE_TIMEOUT: Duration = Duration::from_secs(3);

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
    token: RwLock<Option<String>>,
    session_id: RwLock<Option<String>>,
    running_task: Mutex<Option<JoinHandle<()>>>,
    stop_token: Mutex<Option<CancellationToken>>,
    running: AtomicBool,
    healthy: AtomicBool,
    in_callback: AtomicBool,
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
        Self {
            heartbeat_fn,
            on_force_exit,
            on_update_required,
            heartbeat_interval: HEARTBEAT_INTERVAL,
            request_timeout: HEARTBEAT_REQUEST_TIMEOUT,
            goodbye_timeout: GOODBYE_TIMEOUT,
            state: Arc::new(SessionLifecycleState::default()),
        }
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
        self.state.session_id.read().await.clone()
    }

    pub async fn start(
        &self,
        session_id: String,
        token: String,
    ) -> Result<(), SessionLifecycleError> {
        if self.state.running.load(Ordering::Acquire) {
            return Err(SessionLifecycleError::AlreadyRunning);
        }

        if session_id.trim().is_empty() || token.trim().is_empty() {
            return Err(SessionLifecycleError::Message(
                "session id 与 token 不能为空".to_string(),
            ));
        }

        {
            let mut stored_token = self.state.token.write().await;
            *stored_token = Some(token.clone());
        }
        {
            let mut stored_session_id = self.state.session_id.write().await;
            *stored_session_id = Some(session_id.clone());
        }

        let stop_token = CancellationToken::new();
        {
            let mut current = self.state.stop_token.lock().await;
            *current = Some(stop_token.clone());
        }

        self.state.running.store(true, Ordering::Release);
        let heartbeat_loop = self.spawn_heartbeat_loop(stop_token);
        let mut task = self.state.running_task.lock().await;
        *task = Some(heartbeat_loop);

        Ok(())
    }

    pub async fn stop(&self) -> Result<(), SessionLifecycleError> {
        if !self.state.running.load(Ordering::Acquire) {
            return Err(SessionLifecycleError::NotStarted);
        }

        let stop_token = {
            let mut token = self.state.stop_token.lock().await;
            token.take()
        };

        let handle = {
            let mut task = self.state.running_task.lock().await;
            task.take()
        };

        if let Some(stop_token) = stop_token {
            stop_token.cancel();
        }

        if let Some(handle) = handle {
            if !self.state.in_callback.load(Ordering::Acquire) {
                let _ = handle.await;
            }
        }

        self.send_goodbye().await;
        self.state.running.store(false, Ordering::Release);
        self.state.healthy.store(false, Ordering::Release);
        Ok(())
    }

    async fn send_goodbye(&self) {
        let token = { self.state.token.read().await.clone() };
        let session_id = { self.state.session_id.read().await.clone() };

        let (Some(token), Some(session_id)) = (token, session_id) else {
            return;
        };

        let call = (self.heartbeat_fn)(token, session_id, false);
        let _ = timeout(self.goodbye_timeout, call).await;
    }

    fn spawn_heartbeat_loop(&self, stop_token: CancellationToken) -> JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                if stop_token.is_cancelled() {
                    break;
                }

                let run = this.tick(stop_token.clone()).await;
                if run.should_stop {
                    break;
                }

                if stop_token.is_cancelled() {
                    break;
                }

                sleep(this.heartbeat_interval).await;
            }

            this.state.running.store(false, Ordering::Release);
        })
    }

    async fn tick(&self, stop_token: CancellationToken) -> TickResult {
        let _ = stop_token;
        let token = { self.state.token.read().await.clone() };
        let session_id = { self.state.session_id.read().await.clone() };

        let (Some(token), Some(session_id)) = (token, session_id) else {
            self.state.running.store(false, Ordering::Release);
            return TickResult::stop();
        };

        let heartbeat = (self.heartbeat_fn)(token, session_id, true);
        let response = timeout(self.request_timeout, heartbeat).await;
        match response {
            Ok(Ok(heartbeat)) => {
                self.state.healthy.store(true, Ordering::Release);
                if heartbeat.force_exit {
                    self.state.in_callback.store(true, Ordering::Release);
                    self.state.running.store(false, Ordering::Release);
                    self.send_goodbye().await;
                    self.trigger_force_exit(
                        heartbeat
                            .reason
                            .unwrap_or_else(|| "服务端要求退出".to_string()),
                    );
                    self.state.in_callback.store(false, Ordering::Release);
                    return TickResult::stop();
                }
            }
            Ok(Err(error)) => match error {
                CloudflareError::UpdateRequired(update) => {
                    self.state.in_callback.store(true, Ordering::Release);
                    self.state.running.store(false, Ordering::Release);
                    self.send_goodbye().await;
                    self.trigger_update_required(update);
                    self.state.in_callback.store(false, Ordering::Release);
                    return TickResult::stop();
                }
                CloudflareError::ApiError { status: 401, .. }
                | CloudflareError::ApiError { status: 403, .. } => {
                    self.state.healthy.store(false, Ordering::Release);
                    self.state.in_callback.store(true, Ordering::Release);
                    self.state.running.store(false, Ordering::Release);
                    self.send_goodbye().await;
                    self.trigger_force_exit("账号已被停用或登录失效".to_string());
                    self.state.in_callback.store(false, Ordering::Release);
                    return TickResult::stop();
                }
                _ => {
                    self.state.healthy.store(false, Ordering::Release);
                }
            },
            Err(_) => {
                self.state.healthy.store(false, Ordering::Release);
            }
        }

        TickResult::continue_()
    }

    fn trigger_force_exit(&self, reason: String) {
        if let Some(callback) = self.on_force_exit.as_ref() {
            let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(reason)));
            let _ = outcome;
        }
    }

    fn trigger_update_required(&self, update: UpdateRequiredInfo) {
        if let Some(callback) = self.on_update_required.as_ref() {
            let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| callback(update)));
            let _ = outcome;
        }
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
