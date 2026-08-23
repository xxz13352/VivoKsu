use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use futures::future::{BoxFuture, FutureExt};
use nwflash_infrastructure::{
    CloudflareClient, CloudflareError, CloudflareResult, IntegrityReportPhase,
    IntegrityReportReason, IntegrityReportRequest, SecretToken,
};
use tokio::time::{timeout_at, Instant};

trait IntegrityReportTransport: Send + Sync {
    fn report(
        &self,
        token: Option<SecretToken>,
        request: IntegrityReportRequest,
    ) -> BoxFuture<'static, CloudflareResult<()>>;
}

impl IntegrityReportTransport for CloudflareClient {
    fn report(
        &self,
        token: Option<SecretToken>,
        request: IntegrityReportRequest,
    ) -> BoxFuture<'static, CloudflareResult<()>> {
        let client = self.clone();
        async move { client.report_integrity(token.as_ref(), &request).await }.boxed()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityReportOutcome {
    Accepted,
    HttpRejected(u16),
    Transport,
    Integrity,
    InvalidInput,
    TimedOut,
    SkippedSameChannelSpkiMismatch,
}

#[derive(Clone)]
pub struct IntegrityReporter {
    transport: Arc<dyn IntegrityReportTransport>,
    session_token: Arc<RwLock<Option<SecretToken>>>,
    client_version: String,
    build_id: String,
    sequence: Arc<AtomicU64>,
    marker_path: Option<PathBuf>,
}

impl IntegrityReporter {
    pub fn new(
        client: CloudflareClient,
        session_token: Arc<RwLock<Option<SecretToken>>>,
        client_version: String,
        build_id: String,
    ) -> Self {
        Self::new_with_transport_and_marker(
            Arc::new(client),
            session_token,
            client_version,
            build_id,
            default_marker_path(),
        )
    }

    #[cfg(test)]
    fn new_with_transport(
        transport: Arc<dyn IntegrityReportTransport>,
        session_token: Arc<RwLock<Option<SecretToken>>>,
        client_version: String,
        build_id: String,
    ) -> Self {
        Self::new_with_transport_and_marker(
            transport,
            session_token,
            client_version,
            build_id,
            None,
        )
    }

    fn new_with_transport_and_marker(
        transport: Arc<dyn IntegrityReportTransport>,
        session_token: Arc<RwLock<Option<SecretToken>>>,
        client_version: String,
        build_id: String,
        marker_path: Option<PathBuf>,
    ) -> Self {
        Self {
            transport,
            session_token,
            client_version,
            build_id,
            sequence: Arc::new(AtomicU64::new(0)),
            marker_path,
        }
    }

    pub async fn report_once(
        &self,
        phase: IntegrityReportPhase,
        reason: IntegrityReportReason,
        deadline: Instant,
    ) -> IntegrityReportOutcome {
        self.report_once_with_authentication(phase, reason, deadline, true)
            .await
    }

    pub(crate) async fn report_once_with_authentication(
        &self,
        phase: IntegrityReportPhase,
        reason: IntegrityReportReason,
        deadline: Instant,
        authenticated: bool,
    ) -> IntegrityReportOutcome {
        if deadline <= Instant::now() {
            return IntegrityReportOutcome::TimedOut;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed);
        let event_id = format!("integrity-{}-{sequence}", now.as_millis());
        let occurred_at = i64::try_from(now.as_secs()).unwrap_or(i64::MAX).max(1);
        if phase == IntegrityReportPhase::PinValidation
            && reason == IntegrityReportReason::PinMismatch
        {
            if let Some(marker_path) = self.marker_path.as_deref() {
                let marker_path = marker_path.to_path_buf();
                let marker_event_id = event_id.clone();
                let marker = tokio::task::spawn_blocking(move || {
                    write_minimal_marker(&marker_path, &marker_event_id, occurred_at, sequence)
                });
                let _ = timeout_at(deadline, marker).await;
            }
            return IntegrityReportOutcome::SkippedSameChannelSpkiMismatch;
        }

        let token = authenticated
            .then(|| {
                self.session_token
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .map(SecretToken::request_scope)
            })
            .flatten();
        let request = IntegrityReportRequest {
            event_id,
            phase,
            reason,
            client_version: self.client_version.clone(),
            build_id: self.build_id.clone(),
            occurred_at,
        };

        match timeout_at(deadline, self.transport.report(token, request)).await {
            Err(_) => IntegrityReportOutcome::TimedOut,
            Ok(Ok(())) => IntegrityReportOutcome::Accepted,
            Ok(Err(CloudflareError::ApiError { status, .. })) => {
                IntegrityReportOutcome::HttpRejected(status)
            }
            Ok(Err(CloudflareError::UpdateRequired(_))) => {
                IntegrityReportOutcome::HttpRejected(426)
            }
            Ok(Err(CloudflareError::Integrity(_))) => IntegrityReportOutcome::Integrity,
            Ok(Err(CloudflareError::InvalidInput(_))) => IntegrityReportOutcome::InvalidInput,
            Ok(Err(CloudflareError::Transport(_) | CloudflareError::InvalidResponse(_))) => {
                IntegrityReportOutcome::Transport
            }
        }
    }
}

fn default_marker_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .map(|root| root.join("VivoKsu").join("integrity-tamper.json"))
}

fn write_minimal_marker(
    path: &Path,
    event_id: &str,
    occurred_at: i64,
    sequence: u64,
) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("tmp-{}-{sequence}", std::process::id()));
    let body = format!(
        "{{\"schema_version\":1,\"event_id\":\"{event_id}\",\"phase\":\"pin_validation\",\"reason\":\"pin_mismatch\",\"occurred_at\":{occurred_at}}}"
    );
    std::fs::write(&temporary, body)?;
    if std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temporary, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex, RwLock},
        time::Duration,
    };

    use futures::future::{BoxFuture, FutureExt};
    use nwflash_infrastructure::{
        CloudflareError, CloudflareResult, IntegrityReportPhase, IntegrityReportReason,
        IntegrityReportRequest, SecretToken,
    };
    use tokio::time::Instant;

    use super::*;

    #[derive(Clone)]
    struct RecordingTransport {
        calls: Arc<Mutex<Vec<(bool, IntegrityReportRequest)>>>,
        behavior: TransportBehavior,
    }

    #[derive(Clone, Copy)]
    enum TransportBehavior {
        Accepted,
        HttpRejected(u16),
        Pending,
    }

    impl RecordingTransport {
        fn new(behavior: TransportBehavior) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                behavior,
            }
        }
    }

    impl IntegrityReportTransport for RecordingTransport {
        fn report(
            &self,
            token: Option<SecretToken>,
            request: IntegrityReportRequest,
        ) -> BoxFuture<'static, CloudflareResult<()>> {
            self.calls.lock().unwrap().push((token.is_some(), request));
            match self.behavior {
                TransportBehavior::Accepted => futures::future::ready(Ok(())).boxed(),
                TransportBehavior::HttpRejected(status) => {
                    futures::future::ready(Err(CloudflareError::ApiError {
                        status,
                        message: "fixed-redacted".to_string(),
                    }))
                    .boxed()
                }
                TransportBehavior::Pending => futures::future::pending().boxed(),
            }
        }
    }

    fn reporter(transport: RecordingTransport, token: Option<SecretToken>) -> IntegrityReporter {
        IntegrityReporter::new_with_transport(
            Arc::new(transport),
            Arc::new(RwLock::new(token)),
            "1.0.1".to_string(),
            "build-test".to_string(),
        )
    }

    #[tokio::test]
    async fn report_once_sends_one_closed_request_and_returns_accepted() {
        let transport = RecordingTransport::new(TransportBehavior::Accepted);
        let calls = transport.calls.clone();
        let reporter = reporter(transport, None);

        let outcome = reporter
            .report_once(
                IntegrityReportPhase::Heartbeat,
                IntegrityReportReason::LeaseBindingInvalid,
                Instant::now() + Duration::from_secs(1),
            )
            .await;

        assert_eq!(outcome, IntegrityReportOutcome::Accepted);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(!calls[0].0);
        assert_eq!(calls[0].1.phase, IntegrityReportPhase::Heartbeat);
        assert_eq!(
            calls[0].1.reason,
            IntegrityReportReason::LeaseBindingInvalid
        );
        assert!(calls[0].1.event_id.len() <= 64);
        assert!(calls[0]
            .1
            .event_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')));
    }

    #[tokio::test]
    async fn report_once_captures_owned_token_scope_and_never_retries_http_failure() {
        let transport = RecordingTransport::new(TransportBehavior::HttpRejected(401));
        let calls = transport.calls.clone();
        let reporter = reporter(
            transport,
            Some(SecretToken::new("scoped-report-secret".to_string())),
        );

        let outcome = reporter
            .report_once(
                IntegrityReportPhase::Login,
                IntegrityReportReason::LeaseSignatureInvalid,
                Instant::now() + Duration::from_secs(1),
            )
            .await;

        assert_eq!(outcome, IntegrityReportOutcome::HttpRejected(401));
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0);
        assert!(!format!("{outcome:?}").contains("scoped-report-secret"));
    }

    #[tokio::test]
    async fn report_once_abandons_one_pending_attempt_at_the_supplied_deadline() {
        let transport = RecordingTransport::new(TransportBehavior::Pending);
        let calls = transport.calls.clone();
        let reporter = reporter(transport, None);

        let outcome = reporter
            .report_once(
                IntegrityReportPhase::Startup,
                IntegrityReportReason::ImageCrcInvalid,
                Instant::now() + Duration::from_millis(20),
            )
            .await;

        assert_eq!(outcome, IntegrityReportOutcome::TimedOut);
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn report_once_does_not_start_io_after_the_absolute_deadline() {
        let transport = RecordingTransport::new(TransportBehavior::Accepted);
        let calls = transport.calls.clone();
        let reporter = reporter(transport, None);

        let outcome = reporter
            .report_once(
                IntegrityReportPhase::Startup,
                IntegrityReportReason::ImageCrcInvalid,
                Instant::now() - Duration::from_millis(1),
            )
            .await;

        assert_eq!(outcome, IntegrityReportOutcome::TimedOut);
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn pin_mismatch_writes_minimal_marker_and_skips_same_channel_post() {
        let transport = RecordingTransport::new(TransportBehavior::Accepted);
        let calls = transport.calls.clone();
        let marker_root =
            std::env::temp_dir().join(format!("nwflash-integrity-marker-{}", std::process::id()));
        let marker_path = marker_root.join("marker.json");
        let _ = std::fs::remove_dir_all(&marker_root);
        let reporter = IntegrityReporter::new_with_transport_and_marker(
            Arc::new(transport),
            Arc::new(RwLock::new(Some(SecretToken::new(
                "marker-token-secret".to_string(),
            )))),
            "1.0.1".to_string(),
            "build-test".to_string(),
            Some(marker_path.clone()),
        );

        let outcome = reporter
            .report_once(
                IntegrityReportPhase::PinValidation,
                IntegrityReportReason::PinMismatch,
                Instant::now() + Duration::from_secs(1),
            )
            .await;

        assert_eq!(
            outcome,
            IntegrityReportOutcome::SkippedSameChannelSpkiMismatch
        );
        assert!(calls.lock().unwrap().is_empty());
        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(marker.as_object().unwrap().len(), 5);
        assert_eq!(marker["schema_version"], 1);
        assert_eq!(marker["phase"], "pin_validation");
        assert_eq!(marker["reason"], "pin_mismatch");
        let marker_text = marker.to_string();
        for prohibited in [
            "token",
            "path",
            "url",
            "serial",
            "output",
            "marker-token-secret",
        ] {
            assert!(!marker_text.contains(prohibited));
        }
        let _ = std::fs::remove_dir_all(marker_root);
    }
}
