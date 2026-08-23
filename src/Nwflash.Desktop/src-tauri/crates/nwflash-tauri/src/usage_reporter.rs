//! Best-effort batch usage-log reporter.

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use nwflash_application::UsageReporter;
use nwflash_domain::UsageLogEntry;
use nwflash_infrastructure::{CloudflareClient, SecretToken};
use tauri::async_runtime;
use tokio::{
    sync::Mutex as AsyncMutex,
    time::{interval, timeout_at, Instant},
};
use tokio_util::sync::CancellationToken;

use crate::exit_supervisor::ExitUsageCloseout;

const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const FLUSH_THRESHOLD: usize = 20;
const MAX_BATCH_SIZE: usize = 100;

#[derive(Debug)]
pub struct UsageLogReporter {
    client: CloudflareClient,
    session_token: Arc<RwLock<Option<SecretToken>>>,
    pending: Arc<Mutex<VecDeque<UsageLogEntry>>>,
    flush_gate: Arc<AsyncMutex<()>>,
    running: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    close_token: CancellationToken,
}

impl Clone for UsageLogReporter {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            session_token: self.session_token.clone(),
            pending: self.pending.clone(),
            flush_gate: self.flush_gate.clone(),
            running: self.running.clone(),
            stopped: self.stopped.clone(),
            close_token: self.close_token.clone(),
        }
    }
}

impl UsageLogReporter {
    pub fn new(
        client: CloudflareClient,
        session_token: Arc<RwLock<Option<SecretToken>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            session_token,
            pending: Arc::new(Mutex::new(VecDeque::new())),
            flush_gate: Arc::new(AsyncMutex::new(())),
            running: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            close_token: CancellationToken::new(),
        })
    }

    pub fn start(self: &Arc<Self>) {
        if self.stopped.load(Ordering::Acquire) || self.running.swap(true, Ordering::AcqRel) {
            return;
        }

        let this = self.clone();
        async_runtime::spawn(async move {
            let mut tick = interval(FLUSH_INTERVAL);
            tick.tick().await;
            loop {
                if this.stopped.load(Ordering::Acquire) {
                    break;
                }
                this.flush().await;
                tick.tick().await;
            }
        });
    }

    pub async fn flush(self: &Arc<Self>) {
        let _guard = self.flush_gate.lock().await;

        if self.stopped.load(Ordering::Acquire) {
            return;
        }

        let batch = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if pending.is_empty() {
                return;
            }

            let mut batch = Vec::new();
            while let Some(entry) = pending.pop_front() {
                batch.push(entry);
            }
            batch
        };

        let token = self
            .session_token
            .read()
            .ok()
            .and_then(|token| token.as_ref().map(SecretToken::request_scope));
        let Some(token) = token else {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for entry in batch.into_iter().rev() {
                pending.push_front(entry);
            }
            return;
        };

        let mut failed = Vec::new();
        let mut offset = 0;

        while offset < batch.len() {
            let end = (offset + MAX_BATCH_SIZE).min(batch.len());
            let chunk = &batch[offset..end];

            let upload = self.client.upload_usage_logs(token.as_str(), chunk);
            let failed_or_closed = tokio::select! {
                _ = self.close_token.cancelled() => true,
                result = upload => result.is_err(),
            };
            if failed_or_closed {
                failed.extend_from_slice(&batch[offset..end]);
                break;
            }

            offset = end;
        }

        if !failed.is_empty() {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for entry in failed.into_iter().rev() {
                pending.push_front(entry);
            }
        }
    }

    pub async fn shutdown(self: &Arc<Self>) {
        self.flush().await;
        self.disable_after_flush_attempt();
    }

    pub async fn flush_and_disable_until(self: &Arc<Self>, deadline: Instant) {
        if deadline > Instant::now() {
            let _ = timeout_at(deadline, self.flush()).await;
        }
        self.disable_after_flush_attempt();
    }

    pub fn disable_after_flush_attempt(&self) {
        self.stopped.store(true, Ordering::Release);
        self.running.store(false, Ordering::Release);
        self.close_token.cancel();
    }

    pub fn start_if_needed(self: &Arc<Self>) {
        self.start();
    }

    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }
}

impl UsageReporter for UsageLogReporter {
    fn record(&self, entry: UsageLogEntry) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }

        let should_flush = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            pending.push_back(entry);
            pending.len() >= FLUSH_THRESHOLD
        };

        if should_flush {
            let reporter = Arc::new(self.clone());
            std::mem::drop(async_runtime::spawn(async move { reporter.flush().await }));
        }
    }
}

pub(crate) struct UsageExitCloseout {
    reporter: Arc<UsageLogReporter>,
}

impl UsageExitCloseout {
    pub(crate) fn new(reporter: Arc<UsageLogReporter>) -> Self {
        Self { reporter }
    }
}

impl ExitUsageCloseout for UsageExitCloseout {
    fn flush_until(&self, deadline: Instant) -> futures::future::BoxFuture<'static, ()> {
        let reporter = self.reporter.clone();
        Box::pin(async move {
            reporter.flush_and_disable_until(deadline).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, RwLock},
        time::Duration,
    };

    use nwflash_application::UsageReporter;
    use nwflash_domain::UsageLogEntry;
    use nwflash_infrastructure::{CloudflareClient, SecretToken, DEFAULT_APP_VERSION};
    use tokio::time::Instant;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use super::UsageLogReporter;

    fn usage_entry(event_id: &str) -> UsageLogEntry {
        UsageLogEntry {
            operation: "Flashing".to_string(),
            title: "controlled".to_string(),
            status: "success".to_string(),
            event_id: event_id.to_string(),
            started_at: 1,
            ended_at: Some(2),
            duration_ms: Some(1_000),
        }
    }

    #[tokio::test]
    async fn bounded_closeout_flushes_before_disabling_new_records() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/usage/logs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "received": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        let token = Arc::new(RwLock::new(Some(SecretToken::new(
            "usage-token".to_string(),
        ))));
        let reporter = UsageLogReporter::new(
            CloudflareClient::new_injected(server.uri(), DEFAULT_APP_VERSION),
            token,
        );
        reporter.record(usage_entry("before-close"));

        reporter
            .flush_and_disable_until(Instant::now() + Duration::from_secs(1))
            .await;

        assert_eq!(reporter.pending_count(), 0);
        reporter.record(usage_entry("after-close"));
        assert_eq!(reporter.pending_count(), 0);
    }

    #[test]
    fn threshold_flush_clone_observes_shared_disable_state() {
        let reporter = UsageLogReporter::new(
            CloudflareClient::new_injected("http://127.0.0.1:1", DEFAULT_APP_VERSION),
            Arc::new(RwLock::new(None)),
        );
        let threshold_clone = Arc::new(reporter.as_ref().clone());

        reporter.disable_after_flush_attempt();
        threshold_clone.record(usage_entry("after-disable"));

        assert_eq!(threshold_clone.pending_count(), 0);
    }
}
