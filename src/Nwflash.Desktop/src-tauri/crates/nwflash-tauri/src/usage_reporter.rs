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
use tokio::{sync::Mutex as AsyncMutex, time::interval};

const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const FLUSH_THRESHOLD: usize = 20;
const MAX_BATCH_SIZE: usize = 100;

#[derive(Debug)]
pub struct UsageLogReporter {
    client: CloudflareClient,
    session_token: Arc<RwLock<Option<SecretToken>>>,
    pending: Arc<Mutex<VecDeque<UsageLogEntry>>>,
    flush_gate: Arc<AsyncMutex<()>>,
    running: AtomicBool,
    stopped: AtomicBool,
}

impl Clone for UsageLogReporter {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            session_token: self.session_token.clone(),
            pending: self.pending.clone(),
            flush_gate: self.flush_gate.clone(),
            running: AtomicBool::new(self.running.load(Ordering::Acquire)),
            stopped: AtomicBool::new(self.stopped.load(Ordering::Acquire)),
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
            running: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
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

            if self
                .client
                .upload_usage_logs(token.as_str(), chunk)
                .await
                .is_err()
            {
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
        self.stopped.store(true, Ordering::Release);
        self.flush().await;
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
