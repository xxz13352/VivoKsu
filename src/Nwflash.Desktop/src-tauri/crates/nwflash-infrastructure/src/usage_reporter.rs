//! V1 usage-log retirement bridge.
//!
//! This module deliberately remains separate from the Plan C metadata spool.
//! It is a compatibility queue for the old `/api/usage/logs` payload only;
//! producers must migrate to the protection-sealed trace producer before this
//! bridge can be removed. No raw V1 entry is converted into a V2 trace item.

use nwflash_domain::UsageLogEntry;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock};
use tokio::time::{timeout_at, Instant};

const MAX_BATCH_SIZE: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReporterOwner {
    account: String,
    generation: u64,
}

impl ReporterOwner {
    pub fn new(account: String, generation: u64) -> Self {
        Self {
            account,
            generation,
        }
    }

    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn same_account(&self, other: &Self) -> bool {
        self.account == other.account
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadError {
    Transient,
    Permanent,
}

pub trait UsageUploadTransport: Send + Sync + 'static {
    fn upload<'a>(
        &'a self,
        owner: &'a ReporterOwner,
        entries: &'a [UsageLogEntry],
    ) -> Pin<Box<dyn Future<Output = Result<(), UploadError>> + Send + 'a>>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct QueuedEntry {
    owner: ReporterOwner,
    entry: UsageLogEntry,
}

#[derive(Debug)]
pub enum LegacyUsageReporterError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for LegacyUsageReporterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("legacy usage spool I/O failed"),
            Self::Json(_) => formatter.write_str("legacy usage spool JSON is invalid"),
        }
    }
}

impl std::error::Error for LegacyUsageReporterError {}

impl From<std::io::Error> for LegacyUsageReporterError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for LegacyUsageReporterError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub struct LegacyUsageReporter<T> {
    path: Arc<PathBuf>,
    transport: T,
    owner: Arc<RwLock<ReporterOwner>>,
    pending: Arc<Mutex<Vec<QueuedEntry>>>,
    flush_gate: Arc<tokio::sync::Mutex<()>>,
}

impl<T> LegacyUsageReporter<T>
where
    T: UsageUploadTransport,
{
    pub fn open(
        path: impl Into<PathBuf>,
        transport: T,
        owner: ReporterOwner,
    ) -> Result<Self, LegacyUsageReporterError> {
        let path = path.into();
        let pending = if path.is_file() {
            let bytes = std::fs::read(&path)?;
            serde_json::from_slice(&bytes)?
        } else {
            Vec::new()
        };
        Ok(Self {
            path: Arc::new(path),
            transport,
            owner: Arc::new(RwLock::new(owner)),
            pending: Arc::new(Mutex::new(pending)),
            flush_gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn enqueue(&self, entry: UsageLogEntry) -> Result<(), LegacyUsageReporterError> {
        let owner = self
            .owner
            .read()
            .expect("owner lock should not be poisoned")
            .clone();
        let mut pending = self
            .pending
            .lock()
            .expect("pending lock should not be poisoned");
        pending.push(QueuedEntry { owner, entry });
        persist(&self.path, &pending)
    }

    pub fn set_owner(&self, owner: ReporterOwner) -> Result<(), LegacyUsageReporterError> {
        *self
            .owner
            .write()
            .expect("owner lock should not be poisoned") = owner;
        Ok(())
    }

    pub fn pending_count(&self) -> Result<usize, LegacyUsageReporterError> {
        Ok(self
            .pending
            .lock()
            .expect("pending lock should not be poisoned")
            .len())
    }

    pub async fn flush(&self) -> Result<(), LegacyUsageReporterError> {
        let _guard = self.flush_gate.lock().await;
        let owner = self
            .owner
            .read()
            .expect("owner lock should not be poisoned")
            .clone();
        loop {
            let batch = {
                let pending = self
                    .pending
                    .lock()
                    .expect("pending lock should not be poisoned");
                let indices: Vec<_> = pending
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item.owner.same_account(&owner))
                    .map(|(index, _)| index)
                    .take(MAX_BATCH_SIZE)
                    .collect();
                if indices.is_empty() {
                    return Ok(());
                }
                indices
                    .iter()
                    .map(|index| pending[*index].entry.clone())
                    .collect::<Vec<_>>()
            };

            if self.transport.upload(&owner, &batch).await.is_err() {
                // Leave the full batch, including every unattempted tail, on disk.
                return Ok(());
            }

            let mut pending = self
                .pending
                .lock()
                .expect("pending lock should not be poisoned");
            let mut removed = 0;
            pending.retain(|item| {
                if removed < batch.len() && item.owner.same_account(&owner) {
                    removed += 1;
                    false
                } else {
                    true
                }
            });
            persist(&self.path, &pending)?;
        }
    }

    pub async fn flush_until(&self, deadline: Instant) {
        if deadline > Instant::now() {
            let _ = timeout_at(deadline, self.flush()).await;
        }
    }
}

fn persist(path: &Path, pending: &[QueuedEntry]) -> Result<(), LegacyUsageReporterError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(pending)?;
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };

    use nwflash_domain::UsageLogEntry;
    use tokio::time::Instant;

    use super::{LegacyUsageReporter, ReporterOwner, UploadError, UsageUploadTransport};

    fn entry(id: usize) -> UsageLogEntry {
        UsageLogEntry {
            operation: "Flashing".into(),
            title: "compatibility".into(),
            status: "success".into(),
            event_id: format!("event-{id}"),
            started_at: id as i64,
            ended_at: Some(id as i64 + 1),
            duration_ms: Some(1),
        }
    }

    type UploadCall = (ReporterOwner, Vec<UsageLogEntry>);

    #[derive(Clone)]
    struct ScriptedTransport {
        calls: Arc<Mutex<Vec<UploadCall>>>,
        fail_call: usize,
    }

    impl UsageUploadTransport for ScriptedTransport {
        fn upload<'a>(
            &'a self,
            owner: &'a ReporterOwner,
            entries: &'a [UsageLogEntry],
        ) -> Pin<Box<dyn Future<Output = Result<(), UploadError>> + Send + 'a>> {
            let calls = self.calls.clone();
            let fail = self.fail_call == calls.lock().unwrap().len() + 1;
            let owner = owner.clone();
            let entries = entries.to_vec();
            Box::pin(async move {
                calls.lock().unwrap().push((owner, entries));
                if fail {
                    Err(UploadError::Transient)
                } else {
                    Ok(())
                }
            })
        }
    }

    fn owner(account: &str, generation: u64) -> ReporterOwner {
        ReporterOwner::new(account.to_owned(), generation)
    }

    #[tokio::test]
    async fn failed_middle_batch_retains_failed_and_unattempted_tail_durably() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedTransport {
            calls: calls.clone(),
            fail_call: 2,
        };
        let reporter =
            LegacyUsageReporter::open(root.path().join("v1.json"), transport, owner("a", 1))
                .unwrap();
        for id in 0..250 {
            reporter.enqueue(entry(id)).unwrap();
        }

        reporter.flush().await.unwrap();

        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(reporter.pending_count().unwrap(), 150);
        let reopened = LegacyUsageReporter::open(
            root.path().join("v1.json"),
            ScriptedTransport {
                calls: Arc::new(Mutex::new(Vec::new())),
                fail_call: 0,
            },
            owner("a", 1),
        )
        .unwrap();
        assert_eq!(reopened.pending_count().unwrap(), 150);
    }

    #[tokio::test]
    async fn an_old_owner_queue_is_not_uploaded_by_a_different_owner() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedTransport {
            calls: calls.clone(),
            fail_call: 0,
        };
        let reporter =
            LegacyUsageReporter::open(root.path().join("v1.json"), transport, owner("a", 1))
                .unwrap();
        reporter.enqueue(entry(1)).unwrap();
        reporter.set_owner(owner("b", 1)).unwrap();

        reporter.flush().await.unwrap();

        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(reporter.pending_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn a_new_generation_for_the_same_account_can_resume_the_queue() {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let transport = ScriptedTransport {
            calls: calls.clone(),
            fail_call: 0,
        };
        let reporter =
            LegacyUsageReporter::open(root.path().join("v1.json"), transport, owner("a", 1))
                .unwrap();
        reporter.enqueue(entry(1)).unwrap();
        reporter.set_owner(owner("a", 2)).unwrap();

        reporter.flush().await.unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, owner("a", 2));
        assert_eq!(calls[0].1[0].event_id, "event-1");
        assert_eq!(reporter.pending_count().unwrap(), 0);
    }

    #[tokio::test]
    async fn deadline_cancellation_leaves_inflight_data_durable() {
        let root = tempfile::tempdir().unwrap();
        let transport = HangingTransport;
        let reporter =
            LegacyUsageReporter::open(root.path().join("v1.json"), transport, owner("a", 1))
                .unwrap();
        reporter.enqueue(entry(1)).unwrap();

        reporter
            .flush_until(Instant::now() + Duration::from_millis(10))
            .await;

        assert_eq!(reporter.pending_count().unwrap(), 1);
        let reopened =
            LegacyUsageReporter::open(root.path().join("v1.json"), HangingTransport, owner("a", 1))
                .unwrap();
        assert_eq!(reopened.pending_count().unwrap(), 1);
    }

    struct HangingTransport;

    impl UsageUploadTransport for HangingTransport {
        fn upload<'a>(
            &'a self,
            _owner: &'a ReporterOwner,
            _entries: &'a [UsageLogEntry],
        ) -> Pin<Box<dyn Future<Output = Result<(), UploadError>> + Send + 'a>> {
            Box::pin(std::future::pending())
        }
    }
}
