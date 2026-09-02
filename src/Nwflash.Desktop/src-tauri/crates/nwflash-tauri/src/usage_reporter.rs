//! Durable Tauri adapter for the retiring V1 usage-log endpoint.

use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use nwflash_application::UsageReporter;
use nwflash_domain::UsageLogEntry;
use nwflash_infrastructure::{
    usage_reporter::{
        LegacyUsageReporter, LegacyUsageReporterError, ReporterOwner, UploadError,
        UsageUploadTransport,
    },
    CloudflareClient, SecretToken,
};
use sha2::{Digest, Sha256};
use tauri::async_runtime;
use tokio::{
    sync::Mutex as AsyncMutex,
    time::{interval, timeout_at, Instant},
};
use tokio_util::sync::CancellationToken;

use crate::exit_supervisor::ExitUsageCloseout;

const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const FLUSH_THRESHOLD: usize = 20;

struct BoundCredential {
    owner: ReporterOwner,
    lifecycle_generation: String,
    token: SecretToken,
    cancel: CancellationToken,
}

#[derive(Clone)]
struct CloudflareLegacyTransport {
    client: CloudflareClient,
    credential: Arc<RwLock<Option<BoundCredential>>>,
}

impl UsageUploadTransport for CloudflareLegacyTransport {
    fn upload<'a>(
        &'a self,
        owner: &'a ReporterOwner,
        entries: &'a [UsageLogEntry],
    ) -> Pin<Box<dyn Future<Output = Result<(), UploadError>> + Send + 'a>> {
        let credential = self.credential.read().ok().and_then(|credential| {
            credential
                .as_ref()
                .filter(|credential| credential.owner == *owner)
                .map(|credential| (credential.token.request_scope(), credential.cancel.clone()))
        });
        let client = self.client.clone();
        Box::pin(async move {
            let Some((token, cancel)) = credential else {
                return Err(UploadError::Transient);
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(UploadError::Transient),
                result = client.upload_usage_logs(token.as_str(), entries) => {
                    result.map(|_| ()).map_err(|error| match error.status_code() {
                        Some(400..=499) => UploadError::Permanent,
                        _ => UploadError::Transient,
                    })
                }
            }
        })
    }
}

pub struct UsageLogReporter {
    durable: Arc<LegacyUsageReporter<CloudflareLegacyTransport>>,
    credential: Arc<RwLock<Option<BoundCredential>>>,
    transition_gate: Arc<AsyncMutex<()>>,
    owner_gate: Arc<Mutex<()>>,
    running: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    worker_cancel: CancellationToken,
}

struct ReporterCloseoutGuard {
    reporter: Arc<UsageLogReporter>,
}

impl Drop for ReporterCloseoutGuard {
    fn drop(&mut self) {
        self.reporter.finish_closeout();
    }
}

impl std::fmt::Debug for UsageLogReporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UsageLogReporter")
            .field("durable", &true)
            .field("running", &self.running.load(Ordering::Acquire))
            .field("stopped", &self.stopped.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl Clone for UsageLogReporter {
    fn clone(&self) -> Self {
        Self {
            durable: self.durable.clone(),
            credential: self.credential.clone(),
            transition_gate: self.transition_gate.clone(),
            owner_gate: self.owner_gate.clone(),
            running: self.running.clone(),
            stopped: self.stopped.clone(),
            worker_cancel: self.worker_cancel.clone(),
        }
    }
}

impl UsageLogReporter {
    pub fn new(client: CloudflareClient) -> Result<Arc<Self>, LegacyUsageReporterError> {
        Self::open_at(client, default_spool_path())
    }

    pub(crate) fn open_at(
        client: CloudflareClient,
        path: impl AsRef<Path>,
    ) -> Result<Arc<Self>, LegacyUsageReporterError> {
        let credential = Arc::new(RwLock::new(None));
        let transport = CloudflareLegacyTransport {
            client,
            credential: credential.clone(),
        };
        let inactive_owner = ReporterOwner::new(opaque_account_owner("inactive"), 0);
        let durable = LegacyUsageReporter::open(path.as_ref(), transport, inactive_owner)?;
        Ok(Arc::new(Self {
            durable: Arc::new(durable),
            credential,
            transition_gate: Arc::new(AsyncMutex::new(())),
            owner_gate: Arc::new(Mutex::new(())),
            running: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            worker_cancel: CancellationToken::new(),
        }))
    }

    pub async fn publish_session(
        &self,
        account: String,
        lifecycle_generation: &str,
        token: SecretToken,
    ) -> Result<(), LegacyUsageReporterError> {
        let owner = ReporterOwner::new(
            opaque_account_owner(&account),
            bridge_generation(lifecycle_generation),
        );
        let _transition = self.transition_gate.lock().await;
        let _owner = self
            .owner_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.durable.set_owner(owner.clone())?;
        let mut credential = self
            .credential
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(previous) = credential.take() {
            previous.cancel.cancel();
        }
        *credential = Some(BoundCredential {
            owner,
            lifecycle_generation: lifecycle_generation.to_owned(),
            token,
            cancel: CancellationToken::new(),
        });
        Ok(())
    }

    pub fn start(self: &Arc<Self>) {
        if self.stopped.load(Ordering::Acquire) || self.running.swap(true, Ordering::AcqRel) {
            return;
        }

        let this = self.clone();
        async_runtime::spawn(async move {
            let mut tick = interval(FLUSH_INTERVAL);
            loop {
                tokio::select! {
                    _ = this.worker_cancel.cancelled() => break,
                    _ = tick.tick() => {
                        if this.stopped.load(Ordering::Acquire) {
                            break;
                        }
                        this.flush().await;
                    }
                }
            }
        });
    }

    pub async fn flush(self: &Arc<Self>) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let _transition = self.transition_gate.lock().await;
        let _ = self.durable.flush().await;
    }

    pub async fn flush_and_close_session(self: &Arc<Self>, expected_generation: Option<&str>) {
        let _transition = self.transition_gate.lock().await;
        let current_matches = self
            .credential
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|credential| {
                expected_generation
                    .map(|expected| credential.lifecycle_generation == expected)
                    .unwrap_or(true)
            });
        if !current_matches {
            return;
        }
        if !self.stopped.load(Ordering::Acquire) {
            let _ = self.durable.flush().await;
        }
        let _owner = self
            .owner_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut credential = self
            .credential
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(credential) = credential.take() {
            credential.cancel.cancel();
        }
    }

    pub async fn shutdown(self: &Arc<Self>) {
        if !self.begin_closeout() {
            return;
        }
        let _closeout = ReporterCloseoutGuard {
            reporter: self.clone(),
        };
        let _transition = self.transition_gate.lock().await;
        let _ = self.durable.flush().await;
    }

    pub async fn flush_and_disable_until(self: &Arc<Self>, deadline: Instant) {
        if !self.begin_closeout() {
            return;
        }
        let _closeout = ReporterCloseoutGuard {
            reporter: self.clone(),
        };
        if deadline > Instant::now() {
            if let Ok(_transition) = timeout_at(deadline, self.transition_gate.lock()).await {
                self.durable.flush_until(deadline).await;
            }
        }
    }

    #[allow(
        dead_code,
        reason = "legacy closeout seam retained while the V1 reporter is retired"
    )]
    fn disable_after_flush_attempt(&self) {
        if self.begin_closeout() {
            self.finish_closeout();
        }
    }

    fn begin_closeout(&self) -> bool {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.running.store(false, Ordering::Release);
        self.worker_cancel.cancel();
        let _owner = self
            .owner_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(credential) = self
            .credential
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .as_mut()
        {
            credential.cancel.cancel();
            credential.cancel = CancellationToken::new();
        }
        true
    }

    fn finish_closeout(&self) {
        let _owner = self
            .owner_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(credential) = self
            .credential
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            credential.cancel.cancel();
        }
    }

    pub fn start_if_needed(self: &Arc<Self>) {
        self.start();
    }

    pub fn pending_count(&self) -> usize {
        self.durable.pending_count().unwrap_or_default()
    }
}

impl UsageReporter for UsageLogReporter {
    fn record(&self, entry: UsageLogEntry) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }

        let _owner = self
            .owner_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self
            .credential
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .is_none()
        {
            return;
        }
        if self.durable.enqueue(entry).is_err() {
            return;
        }
        let should_flush =
            self.running.load(Ordering::Acquire) && self.pending_count() >= FLUSH_THRESHOLD;
        drop(_owner);

        if should_flush {
            let reporter = Arc::new(self.clone());
            std::mem::drop(async_runtime::spawn(async move { reporter.flush().await }));
        }
    }
}

fn opaque_account_owner(account: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nwflash-v1-usage-owner\0");
    hasher.update(account.as_bytes());
    format!("{digest:x}", digest = hasher.finalize())
}

fn bridge_generation(generation: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"nwflash-v1-usage-generation\0");
    hasher.update(generation.as_bytes());
    let digest = hasher.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(prefix).max(1)
}

fn default_spool_path() -> PathBuf {
    #[cfg(test)]
    {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static TEST_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = TEST_PATH_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        std::env::temp_dir()
            .join("nwflash-usage-reporter-tests")
            .join(format!("{}-{sequence}.json", std::process::id()))
    }
    #[cfg(not(test))]
    {
        std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Nwflash")
            .join("v1-usage-retirement.json")
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
        collections::BTreeSet,
        path::Path,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use nwflash_application::{OperationCoordinator, UsageReporter};
    use nwflash_domain::{OperationKind, UsageLogEntry};
    use nwflash_infrastructure::{CloudflareClient, SecretToken, DEFAULT_APP_VERSION};
    use tokio::{
        sync::Notify,
        time::{timeout, Instant},
    };
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, Request, Respond, ResponseTemplate,
    };

    use super::UsageLogReporter;

    type UploadCall = (String, usize);

    #[derive(Clone)]
    struct ScriptedResponder {
        calls: Arc<Mutex<Vec<UploadCall>>>,
        fail_calls: BTreeSet<usize>,
    }

    #[derive(Clone)]
    struct SignalingDelayedResponder {
        started: Arc<Notify>,
        delay: Duration,
    }

    impl Respond for SignalingDelayedResponder {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            self.started.notify_one();
            ResponseTemplate::new(200)
                .set_delay(self.delay)
                .set_body_json(serde_json::json!({"ok": true, "received": 1}))
        }
    }

    impl Respond for ScriptedResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let authorization = request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let count = serde_json::from_slice::<serde_json::Value>(&request.body)
                .ok()
                .and_then(|body| body["logs"].as_array().map(Vec::len))
                .unwrap_or_default();
            let call = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((authorization, count));
                calls.len()
            };
            if self.fail_calls.contains(&call) {
                ResponseTemplate::new(500)
            } else {
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "received": count
                }))
            }
        }
    }

    fn usage_entry(event_id: &str) -> UsageLogEntry {
        UsageLogEntry {
            operation: "Flashing".to_string(),
            title: "controlled".to_string(),
            status: "success".to_string(),
            event_id: event_id.to_string(),
            started_at: 1,
            ended_at: Some(2),
            duration_ms: Some(1_000),
            details: Vec::new(),
        }
    }

    fn open_reporter(server: &MockServer, path: &Path) -> Arc<UsageLogReporter> {
        UsageLogReporter::open_at(
            CloudflareClient::new_injected(server.uri(), DEFAULT_APP_VERSION),
            path,
        )
        .expect("durable reporter")
    }

    async fn publish(
        reporter: &Arc<UsageLogReporter>,
        account: &str,
        generation: &str,
        token: &str,
    ) {
        reporter
            .publish_session(
                account.to_owned(),
                generation,
                SecretToken::new(token.to_owned()),
            )
            .await
            .expect("session publication");
    }

    async fn mount_script(
        server: &MockServer,
        calls: Arc<Mutex<Vec<UploadCall>>>,
        fail_calls: impl IntoIterator<Item = usize>,
    ) {
        Mock::given(method("POST"))
            .and(path("/api/usage/logs"))
            .respond_with(ScriptedResponder {
                calls,
                fail_calls: fail_calls.into_iter().collect(),
            })
            .mount(server)
            .await;
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
        let root = tempfile::tempdir().unwrap();
        let reporter = open_reporter(&server, &root.path().join("usage-v1.json"));
        publish(&reporter, "account-a", "generation-a", "usage-token").await;
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
        let root = tempfile::tempdir().unwrap();
        let reporter = UsageLogReporter::open_at(
            CloudflareClient::new_injected("http://127.0.0.1:1", DEFAULT_APP_VERSION),
            root.path().join("usage-v1.json"),
        )
        .unwrap();
        let threshold_clone = Arc::new(reporter.as_ref().clone());

        reporter.disable_after_flush_attempt();
        threshold_clone.record(usage_entry("after-disable"));

        assert_eq!(threshold_clone.pending_count(), 0);
    }

    #[tokio::test]
    async fn account_a_failed_queue_is_never_uploaded_with_account_b_token() {
        let server = MockServer::start().await;
        let calls = Arc::new(Mutex::new(Vec::new()));
        mount_script(&server, calls.clone(), [1]).await;
        let root = tempfile::tempdir().unwrap();
        let reporter = open_reporter(&server, &root.path().join("usage-v1.json"));

        publish(&reporter, "account-a", "generation-a", "token-a").await;
        reporter.record(usage_entry("owned-by-a"));
        reporter.flush().await;
        assert_eq!(reporter.pending_count(), 1);

        publish(&reporter, "account-b", "generation-b", "token-b").await;
        reporter.flush().await;

        assert_eq!(reporter.pending_count(), 1);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[("Bearer token-a".into(), 1)]
        );
    }

    #[tokio::test]
    async fn same_account_new_generation_resumes_old_failed_queue_with_new_token() {
        let server = MockServer::start().await;
        let calls = Arc::new(Mutex::new(Vec::new()));
        mount_script(&server, calls.clone(), [1]).await;
        let root = tempfile::tempdir().unwrap();
        let reporter = open_reporter(&server, &root.path().join("usage-v1.json"));

        publish(&reporter, "account-a", "generation-a", "token-a1").await;
        reporter.record(usage_entry("retry-on-next-generation"));
        reporter.flush().await;
        publish(&reporter, "account-a", "generation-b", "token-a2").await;
        reporter.flush().await;

        assert_eq!(reporter.pending_count(), 0);
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[("Bearer token-a1".into(), 1), ("Bearer token-a2".into(), 1)]
        );
    }

    #[tokio::test]
    async fn failed_second_batch_of_250_keeps_failed_hundred_and_fifty_tail_durable() {
        let server = MockServer::start().await;
        let calls = Arc::new(Mutex::new(Vec::new()));
        mount_script(&server, calls.clone(), [2]).await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage-v1.json");
        let reporter = open_reporter(&server, &path);
        publish(&reporter, "account-a", "generation-a", "token-a").await;
        for id in 0..250 {
            reporter.record(usage_entry(&format!("event-{id}")));
        }

        reporter.flush().await;

        assert_eq!(reporter.pending_count(), 150);
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .map(|(_, count)| *count)
                .collect::<Vec<_>>(),
            vec![100, 100]
        );
        drop(reporter);
        let reopened = open_reporter(&server, &path);
        assert_eq!(reopened.pending_count(), 150);
    }

    #[tokio::test]
    async fn deadline_cancellation_reopens_the_durable_queue() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/usage/logs"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(1))
                    .set_body_json(serde_json::json!({"ok": true, "received": 1})),
            )
            .mount(&server)
            .await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage-v1.json");
        let reporter = open_reporter(&server, &path);
        publish(&reporter, "account-a", "generation-a", "token-a").await;
        reporter.record(usage_entry("deadline"));

        reporter
            .flush_and_disable_until(Instant::now() + Duration::from_millis(20))
            .await;
        assert_eq!(reporter.pending_count(), 1);
        drop(reporter);

        let reopened = open_reporter(&server, &path);
        assert_eq!(reopened.pending_count(), 1);
    }

    #[tokio::test]
    async fn bounded_closeout_cancels_an_already_running_flush_without_deleting_its_entry() {
        let server = MockServer::start().await;
        let started = Arc::new(Notify::new());
        Mock::given(method("POST"))
            .and(path("/api/usage/logs"))
            .respond_with(SignalingDelayedResponder {
                started: started.clone(),
                delay: Duration::from_secs(1),
            })
            .mount(&server)
            .await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage-v1.json");
        let reporter = open_reporter(&server, &path);
        publish(&reporter, "account-a", "generation-a", "token-a").await;
        reporter.record(usage_entry("already-inflight"));
        let inflight = {
            let reporter = reporter.clone();
            tokio::spawn(async move { reporter.flush().await })
        };
        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("upload should become inflight");

        reporter
            .flush_and_disable_until(Instant::now() + Duration::from_millis(20))
            .await;

        timeout(Duration::from_millis(200), inflight)
            .await
            .expect("credential cancellation should release the inflight flush")
            .unwrap();
        assert_eq!(reporter.pending_count(), 1);
        drop(reporter);
        let reopened = open_reporter(&server, &path);
        assert_eq!(reopened.pending_count(), 1);
    }

    #[tokio::test]
    async fn dropping_the_outer_closeout_future_cancels_inflight_and_disables_new_records() {
        let server = MockServer::start().await;
        let started = Arc::new(Notify::new());
        Mock::given(method("POST"))
            .and(path("/api/usage/logs"))
            .respond_with(SignalingDelayedResponder {
                started: started.clone(),
                delay: Duration::from_secs(1),
            })
            .mount(&server)
            .await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage-v1.json");
        let reporter = open_reporter(&server, &path);
        publish(&reporter, "account-a", "generation-a", "token-a").await;
        reporter.record(usage_entry("outer-drop"));
        let inflight = {
            let reporter = reporter.clone();
            tokio::spawn(async move { reporter.flush().await })
        };
        timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("upload should become inflight");

        let mut closeout =
            Box::pin(reporter.flush_and_disable_until(Instant::now() + Duration::from_secs(1)));
        tokio::select! {
            _ = &mut closeout => panic!("outer deadline should win this fixture"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        drop(closeout);

        timeout(Duration::from_millis(200), inflight)
            .await
            .expect("dropping closeout must cancel the prior inflight upload")
            .unwrap();
        reporter.record(usage_entry("after-outer-drop"));
        assert_eq!(reporter.pending_count(), 1);
        drop(reporter);
        let reopened = open_reporter(&server, &path);
        assert_eq!(reopened.pending_count(), 1);
    }

    #[tokio::test]
    async fn operation_coordinator_records_only_into_the_reopenable_durable_bridge() {
        let server = MockServer::start().await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage-v1.json");
        let reporter = open_reporter(&server, &path);
        publish(&reporter, "account-a", "generation-a", "token-a").await;
        let coordinator = OperationCoordinator::new(None, None, Some(reporter.clone()), None, None);

        coordinator
            .run_async(OperationKind::Flashing, "durable operation", |_, _| async {
                Ok::<(), nwflash_domain::DomainError>(())
            })
            .await
            .unwrap();
        assert_eq!(reporter.pending_count(), 1);
        drop(coordinator);
        drop(reporter);

        let reopened = open_reporter(&server, &path);
        assert_eq!(reopened.pending_count(), 1);
    }

    #[tokio::test]
    async fn persisted_owner_is_opaque_and_never_contains_generation_or_bearer() {
        let server = MockServer::start().await;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage-v1.json");
        let reporter = open_reporter(&server, &path);
        publish(
            &reporter,
            "raw-account-name",
            "raw-generation-value",
            "raw-bearer-value",
        )
        .await;
        reporter.record(usage_entry("opaque-owner"));

        let disk = std::fs::read_to_string(path).unwrap();
        for secret in [
            "raw-account-name",
            "raw-generation-value",
            "raw-bearer-value",
        ] {
            assert!(!disk.contains(secret));
        }
    }

    #[tokio::test]
    async fn stale_generation_close_cannot_detach_the_current_session() {
        let server = MockServer::start().await;
        let calls = Arc::new(Mutex::new(Vec::new()));
        mount_script(&server, calls.clone(), []).await;
        let root = tempfile::tempdir().unwrap();
        let reporter = open_reporter(&server, &root.path().join("usage-v1.json"));
        publish(
            &reporter,
            "account-a",
            "generation-current",
            "token-current",
        )
        .await;

        reporter.record(usage_entry("before-stale-close"));
        reporter
            .flush_and_close_session(Some("generation-stale"))
            .await;
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(reporter.pending_count(), 1);

        reporter.flush().await;
        assert_eq!(calls.lock().unwrap().len(), 1);

        reporter
            .flush_and_close_session(Some("generation-current"))
            .await;
        reporter.record(usage_entry("after-close"));
        reporter.flush().await;
        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(reporter.pending_count(), 0);
    }
}
