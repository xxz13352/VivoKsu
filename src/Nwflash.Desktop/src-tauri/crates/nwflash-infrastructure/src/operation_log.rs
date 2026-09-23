//! Persistent operation log storage with in-memory and disk rolling window.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_domain::{OperationLogEntry, OperationLogLevel};

const DEFAULT_MAX_ENTRIES: usize = 500;
const MAX_LOG_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct OperationLogStore {
    path: Option<PathBuf>,
    max_entries: usize,
    entries: Arc<Mutex<Vec<OperationLogEntry>>>,
}

impl Default for OperationLogStore {
    fn default() -> Self {
        Self::with_default_path(DEFAULT_MAX_ENTRIES)
    }
}

impl OperationLogStore {
    pub fn with_default_path(max_entries: usize) -> Self {
        Self::new(resolve_default_log_path(), max_entries)
    }

    pub fn new(path: Option<PathBuf>, max_entries: usize) -> Self {
        let max_entries = max_entries.max(1);
        let entries = path
            .as_ref()
            .and_then(|path| read_entries_from_file(path, max_entries).ok())
            .unwrap_or_default();

        Self {
            path,
            max_entries,
            entries: Arc::new(Mutex::new(entries)),
        }
    }

    pub fn snapshot(&self) -> Vec<OperationLogEntry> {
        self.entries
            .lock()
            .expect("operation log lock should not be poisoned")
            .clone()
    }

    pub fn clear_memory(&self) {
        self.entries
            .lock()
            .expect("operation log lock should not be poisoned")
            .clear();
    }

    /// Starts a fresh UI session while retaining the append-only disk history.
    pub fn start_new_session(&self) {
        self.clear_memory();
    }

    pub fn write(&self, level: OperationLogLevel, message: String, operation_id: Option<String>) {
        let entry = OperationLogEntry {
            timestamp_utc: unix_timestamp_seconds().unwrap_or(0),
            level,
            message,
            operation_id,
        };

        {
            let mut entries = self
                .entries
                .lock()
                .expect("operation log lock should not be poisoned");

            entries.push(entry.clone());
            if entries.len() > self.max_entries {
                let overflow = entries.len() - self.max_entries;
                entries.drain(0..overflow);
            }
        }

        if let Some(path) = self.path.as_ref() {
            persist_entry(path, &entry);
        }
    }
}

fn unix_timestamp_seconds() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

fn resolve_default_log_path() -> Option<PathBuf> {
    let base_dir = std::env::var("LOCALAPPDATA").ok()?;
    let mut path = PathBuf::from(base_dir);
    path.push("Nwflash");
    path.push("operations.log");
    Some(path)
}

fn read_entries_from_file(
    path: &Path,
    max_entries: usize,
) -> std::io::Result<Vec<OperationLogEntry>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };

    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line_result in reader.lines() {
        let line = line_result?;
        let parsed = serde_json::from_str::<OperationLogEntry>(&line).ok();

        if let Some(entry) = parsed {
            entries.push(entry);
        }
    }

    if entries.len() > max_entries {
        let overflow = entries.len() - max_entries;
        entries.drain(0..overflow);
    }

    Ok(entries)
}

fn persist_entry(path: &Path, entry: &OperationLogEntry) {
    let _ = fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")));

    if should_rotate_file(path) {
        let _ = rotate_file(path);
    }

    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        if let Ok(payload) = serde_json::to_string(entry) {
            let _ = writeln!(file, "{payload}");
        }
    }
}

fn should_rotate_file(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.len() >= MAX_LOG_FILE_BYTES,
        Err(_) => false,
    }
}

fn rotate_file(path: &Path) -> std::io::Result<()> {
    let target = PathBuf::from(format!("{}.1", path.display()));
    let _ = fs::remove_file(&target);
    fs::rename(path, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_memory_removes_the_snapshot_without_erasing_the_persisted_log() {
        let path =
            std::env::temp_dir().join(format!("nwflash-operation-log-{}.log", std::process::id()));
        let _ = fs::remove_file(&path);
        let store = OperationLogStore::new(Some(path.clone()), 10);
        store.write(
            OperationLogLevel::Info,
            "需要清空的会话日志".to_owned(),
            None,
        );

        store.clear_memory();

        assert!(store.snapshot().is_empty());
        assert!(fs::read_to_string(&path)
            .expect("persisted operation log should remain readable")
            .contains("需要清空的会话日志"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn new_session_hides_previous_runs_without_erasing_the_disk_history() {
        let path = std::env::temp_dir().join(format!(
            "nwflash-operation-log-session-{}-{}.log",
            std::process::id(),
            unix_timestamp_seconds().unwrap_or_default()
        ));
        let _ = fs::remove_file(&path);

        let previous_run = OperationLogStore::new(Some(path.clone()), 10);
        previous_run.write(
            OperationLogLevel::Error,
            "上次运行的服务端错误".to_owned(),
            None,
        );

        let current_run = OperationLogStore::new(Some(path.clone()), 10);
        assert_eq!(current_run.snapshot().len(), 1);

        current_run.start_new_session();
        assert!(current_run.snapshot().is_empty());

        current_run.write(OperationLogLevel::Info, "本次会话操作".to_owned(), None);
        let messages = current_run
            .snapshot()
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["本次会话操作"]);

        let persisted = fs::read_to_string(&path).expect("disk history should remain readable");
        assert!(persisted.contains("上次运行的服务端错误"));
        assert!(persisted.contains("本次会话操作"));
        let _ = fs::remove_file(path);
    }
}
