use std::{
    env,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use futures::future::BoxFuture;
use nwflash_infrastructure::{
    CloudflareClient, CloudflareError, CloudflareResult, IntegrityReportPhase,
    IntegrityReportReason, IntegrityReportRequest, SecretToken,
};
use serde::Serialize;
use tokio::time::{timeout_at, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrityReportAuthorization {
    Anonymous,
    CurrentSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegrityReportOutcome {
    Accepted,
    HttpRejected(u16),
    Transport,
    Integrity,
    InvalidInput,
    InvalidResponse,
    TimedOut,
    SkippedSameChannelSpkiMismatch,
}

pub(crate) trait IntegrityReportClient: Send + Sync {
    fn report_integrity(
        &self,
        token: Option<SecretToken>,
        request: IntegrityReportRequest,
    ) -> BoxFuture<'static, CloudflareResult<()>>;
}

impl IntegrityReportClient for CloudflareClient {
    fn report_integrity(
        &self,
        token: Option<SecretToken>,
        request: IntegrityReportRequest,
    ) -> BoxFuture<'static, CloudflareResult<()>> {
        let client = self.clone();
        Box::pin(async move { client.report_integrity(token.as_ref(), &request).await })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntegrityEvent {
    request: IntegrityReportRequest,
}

impl IntegrityEvent {
    #[cfg(test)]
    pub(crate) fn request(&self) -> &IntegrityReportRequest {
        &self.request
    }
}

#[derive(Clone)]
pub(crate) struct IntegrityReporter {
    client: Arc<dyn IntegrityReportClient>,
    session_token: Arc<RwLock<Option<SecretToken>>>,
    client_version: String,
    build_id: String,
    sequence: Arc<AtomicU64>,
    marker_root: PathBuf,
}

impl IntegrityReporter {
    pub(crate) fn new(
        client: CloudflareClient,
        session_token: Arc<RwLock<Option<SecretToken>>>,
        client_version: String,
        build_id: String,
    ) -> Self {
        Self::with_client(
            Arc::new(client),
            session_token,
            client_version,
            build_id,
            default_marker_root(),
        )
    }

    pub(crate) fn with_client(
        client: Arc<dyn IntegrityReportClient>,
        session_token: Arc<RwLock<Option<SecretToken>>>,
        client_version: String,
        build_id: String,
        marker_root: PathBuf,
    ) -> Self {
        Self {
            client,
            session_token,
            client_version,
            build_id,
            sequence: Arc::new(AtomicU64::new(0)),
            marker_root,
        }
    }

    pub(crate) fn prepare_event(
        &self,
        phase: IntegrityReportPhase,
        reason: IntegrityReportReason,
    ) -> IntegrityEvent {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        IntegrityEvent {
            request: IntegrityReportRequest {
                event_id: format!("integrity-{}-{sequence}", now.as_millis()),
                phase,
                reason,
                client_version: self.client_version.clone(),
                build_id: self.build_id.clone(),
                occurred_at: i64::try_from(now.as_secs()).unwrap_or(i64::MAX).max(1),
            },
        }
    }

    pub(crate) async fn report_once(
        &self,
        event: &IntegrityEvent,
        authorization: IntegrityReportAuthorization,
        deadline: Instant,
    ) -> IntegrityReportOutcome {
        if event.request.phase == IntegrityReportPhase::PinValidation
            && event.request.reason == IntegrityReportReason::PinMismatch
        {
            self.write_pin_marker(event, deadline).await;
            return IntegrityReportOutcome::SkippedSameChannelSpkiMismatch;
        }
        if deadline <= Instant::now() {
            return IntegrityReportOutcome::TimedOut;
        }

        let token = match authorization {
            IntegrityReportAuthorization::Anonymous => None,
            IntegrityReportAuthorization::CurrentSession => self
                .session_token
                .read()
                .ok()
                .and_then(|guard| guard.as_ref().map(SecretToken::request_scope)),
        };
        let call = self.client.report_integrity(token, event.request.clone());
        match timeout_at(deadline, call).await {
            Ok(Ok(())) => IntegrityReportOutcome::Accepted,
            Ok(Err(error)) => compact_outcome(error),
            Err(_) => IntegrityReportOutcome::TimedOut,
        }
    }

    async fn write_pin_marker(&self, event: &IntegrityEvent, deadline: Instant) {
        if deadline <= Instant::now() {
            return;
        }
        let marker = PinMismatchMarker {
            schema_version: 1,
            event_id: event.request.event_id.clone(),
            phase: IntegrityReportPhase::PinValidation,
            reason: IntegrityReportReason::PinMismatch,
            occurred_at: event.request.occurred_at,
        };
        let Ok(bytes) = serde_json::to_vec(&marker) else {
            return;
        };
        let root = self.marker_root.clone();
        let event_id = event.request.event_id.clone();
        let _ = timeout_at(deadline, write_marker_atomically(&root, &event_id, &bytes)).await;
    }
}

#[derive(Serialize)]
struct PinMismatchMarker {
    schema_version: u8,
    event_id: String,
    phase: IntegrityReportPhase,
    reason: IntegrityReportReason,
    occurred_at: i64,
}

async fn write_marker_atomically(root: &Path, event_id: &str, bytes: &[u8]) -> std::io::Result<()> {
    tokio::fs::create_dir_all(root).await?;
    let temporary = root.join(format!(".integrity-marker-{event_id}.tmp"));
    let destination = root.join("integrity-marker.json");
    tokio::fs::write(&temporary, bytes).await?;
    if tokio::fs::rename(&temporary, &destination).await.is_err() {
        let _ = tokio::fs::remove_file(&destination).await;
        tokio::fs::rename(&temporary, &destination).await?;
    }
    Ok(())
}

fn compact_outcome(error: CloudflareError) -> IntegrityReportOutcome {
    match error {
        CloudflareError::ApiError { status, .. } => IntegrityReportOutcome::HttpRejected(status),
        CloudflareError::UpdateRequired(_) => IntegrityReportOutcome::HttpRejected(426),
        CloudflareError::Transport(_) => IntegrityReportOutcome::Transport,
        CloudflareError::Integrity(_) => IntegrityReportOutcome::Integrity,
        CloudflareError::InvalidInput(_) => IntegrityReportOutcome::InvalidInput,
        CloudflareError::InvalidResponse(_) => IntegrityReportOutcome::InvalidResponse,
    }
}

fn default_marker_root() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .or_else(|| env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("NWflash")
        .join("integrity")
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{Arc, Mutex, RwLock},
        time::Duration,
    };

    use futures::future::BoxFuture;
    use nwflash_infrastructure::{
        CloudflareError, CloudflareResult, IntegrityReportPhase, IntegrityReportReason,
        IntegrityReportRequest, SecretToken,
    };
    use tokio::time::Instant;

    use super::{
        IntegrityReportAuthorization, IntegrityReportClient, IntegrityReportOutcome,
        IntegrityReporter,
    };

    #[derive(Clone, Copy)]
    enum FakeResult {
        Accepted,
        HttpError,
        Pending,
    }

    struct FakeClient {
        result: FakeResult,
        calls: Arc<Mutex<Vec<(bool, IntegrityReportRequest)>>>,
    }

    impl FakeClient {
        fn new(result: FakeResult) -> Self {
            Self {
                result,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<(bool, IntegrityReportRequest)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl IntegrityReportClient for FakeClient {
        fn report_integrity(
            &self,
            token: Option<SecretToken>,
            request: IntegrityReportRequest,
        ) -> BoxFuture<'static, CloudflareResult<()>> {
            self.calls.lock().unwrap().push((token.is_some(), request));
            match self.result {
                FakeResult::Accepted => Box::pin(async { Ok(()) }),
                FakeResult::HttpError => Box::pin(async {
                    Err(CloudflareError::ApiError {
                        status: 503,
                        message: "fixed fallback".to_string(),
                    })
                }),
                FakeResult::Pending => Box::pin(pending()),
            }
        }
    }

    fn reporter(client: Arc<FakeClient>, marker_root: &std::path::Path) -> IntegrityReporter {
        IntegrityReporter::with_client(
            client,
            Arc::new(RwLock::new(None)),
            "1.0.1".to_string(),
            "build-test".to_string(),
            marker_root.to_path_buf(),
        )
    }

    #[tokio::test]
    async fn report_success_and_http_error_each_make_exactly_one_attempt() {
        let marker_root = tempfile::tempdir().unwrap();
        for (fake_result, expected) in [
            (FakeResult::Accepted, IntegrityReportOutcome::Accepted),
            (
                FakeResult::HttpError,
                IntegrityReportOutcome::HttpRejected(503),
            ),
        ] {
            let client = Arc::new(FakeClient::new(fake_result));
            let reporter = reporter(client.clone(), marker_root.path());
            let event = reporter.prepare_event(
                IntegrityReportPhase::Heartbeat,
                IntegrityReportReason::LeaseSignatureInvalid,
            );

            let outcome = reporter
                .report_once(
                    &event,
                    IntegrityReportAuthorization::Anonymous,
                    Instant::now() + Duration::from_secs(1),
                )
                .await;

            assert_eq!(outcome, expected);
            assert_eq!(client.calls().len(), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pending_report_uses_the_callers_absolute_deadline_without_retry() {
        let marker_root = tempfile::tempdir().unwrap();
        let client = Arc::new(FakeClient::new(FakeResult::Pending));
        let reporter = reporter(client.clone(), marker_root.path());
        let event = reporter.prepare_event(
            IntegrityReportPhase::Heartbeat,
            IntegrityReportReason::LeaseExpired,
        );
        let deadline = Instant::now() + Duration::from_millis(750);
        let task = tokio::spawn(async move {
            reporter
                .report_once(&event, IntegrityReportAuthorization::Anonymous, deadline)
                .await
        });

        tokio::task::yield_now().await;
        assert_eq!(client.calls().len(), 1);
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(749)).await;
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;

        assert_eq!(task.await.unwrap(), IntegrityReportOutcome::TimedOut);
        assert_eq!(client.calls().len(), 1);
    }

    #[tokio::test]
    async fn current_session_authorization_captures_only_a_request_scoped_token() {
        let marker_root = tempfile::tempdir().unwrap();
        let client = Arc::new(FakeClient::new(FakeResult::Pending));
        let shared = Arc::new(RwLock::new(Some(SecretToken::new(
            "scoped-secret".to_string(),
        ))));
        let reporter = IntegrityReporter::with_client(
            client.clone(),
            shared.clone(),
            "1.0.1".to_string(),
            "build-test".to_string(),
            marker_root.path().to_path_buf(),
        );
        let event = reporter.prepare_event(
            IntegrityReportPhase::Heartbeat,
            IntegrityReportReason::LeaseBindingInvalid,
        );
        let task = tokio::spawn(async move {
            reporter
                .report_once(
                    &event,
                    IntegrityReportAuthorization::CurrentSession,
                    Instant::now() + Duration::from_secs(10),
                )
                .await
        });

        while client.calls().is_empty() {
            tokio::task::yield_now().await;
        }
        shared.write().unwrap().take();
        assert!(shared.read().unwrap().is_none());
        assert!(client.calls()[0].0);
        task.abort();
        task.await.expect_err("pending reporter task should abort");
    }

    #[test]
    fn prepared_event_ids_are_unique_worker_valid_and_body_remains_allowlisted() {
        let marker_root = tempfile::tempdir().unwrap();
        let client = Arc::new(FakeClient::new(FakeResult::Accepted));
        let reporter = reporter(client, marker_root.path());
        let first = reporter.prepare_event(
            IntegrityReportPhase::OperationAdmission,
            IntegrityReportReason::SequenceRollback,
        );
        let second = reporter.prepare_event(
            IntegrityReportPhase::OperationAdmission,
            IntegrityReportReason::SequenceRollback,
        );

        assert_ne!(first.request().event_id, second.request().event_id);
        for event in [&first, &second] {
            let id = &event.request().event_id;
            assert!(!id.is_empty() && id.len() <= 64);
            assert!(id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
            }));
            let body = serde_json::to_value(event.request()).unwrap();
            assert_eq!(body.as_object().unwrap().len(), 6);
        }
    }

    #[tokio::test]
    async fn pin_mismatch_writes_one_minimal_marker_and_skips_same_channel_post() {
        let marker_root = tempfile::tempdir().unwrap();
        let client = Arc::new(FakeClient::new(FakeResult::Accepted));
        let reporter = reporter(client.clone(), marker_root.path());
        let event = reporter.prepare_event(
            IntegrityReportPhase::PinValidation,
            IntegrityReportReason::PinMismatch,
        );

        let outcome = reporter
            .report_once(
                &event,
                IntegrityReportAuthorization::Anonymous,
                Instant::now() + Duration::from_secs(1),
            )
            .await;

        assert_eq!(
            outcome,
            IntegrityReportOutcome::SkippedSameChannelSpkiMismatch
        );
        assert!(client.calls().is_empty());
        let marker = std::fs::read_to_string(marker_root.path().join("integrity-marker.json"))
            .expect("pin mismatch should leave a local marker");
        let marker: serde_json::Value = serde_json::from_str(&marker).unwrap();
        assert_eq!(marker.as_object().unwrap().len(), 5);
        assert_eq!(marker["schema_version"], 1);
        assert_eq!(marker["phase"], "pin_validation");
        assert_eq!(marker["reason"], "pin_mismatch");
        for prohibited in [
            "token", "password", "path", "url", "host", "serial", "output", "spki",
        ] {
            assert!(!marker.to_string().to_ascii_lowercase().contains(prohibited));
        }
    }
}
