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

    /// 追加一条日志，并**跳过与上一条完全相同的消息**。
    ///
    /// 为什么在存储层去重，而不是逐个修调用点：`report_stage` 会被放在轮询/
    /// 重试循环里（例如等待 fastbootd 每秒探测一次就上报一次），于是同一句话
    /// 会以每秒一条的频率把日志区刷满——实测某次会话里
    /// 「正在等待 fastbootd 设备」重复了 788 次。这类重复是**同一处代码的
    /// 同一句文案**在循环里反复触发，逐点修既容易漏、又会在新增循环时回归；
    /// 在唯一的写入收口处按「消息 + 级别 + 操作」判重，才能一次覆盖整类问题。
    ///
    /// 判定范围刻意收窄，避免误吞真实日志：
    /// - 只与**紧邻的上一条**比较（不是全局去重）。同一文案在流程中第二次
    ///   出现时，中间必然隔着别的日志，因此不会被吞掉。
    /// - 必须 `message`、`level`、`operation_id` **三者全同**才算重复。不同
    ///   操作各自记录同一句文案（例如两次刷写）互不影响。
    ///
    /// 注意：这**不是**把日志"合并"成一条带计数的记录——被跳过的重复不改变
    /// 已落盘的那一条，磁盘历史与内存快照行为一致。
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

            // 与**紧邻的上一条**完全同源（消息 + 级别 + 操作全同）时跳过：
            // 这是循环里反复上报同一句文案产生的噪声，不是新的信息。
            if entries.last().is_some_and(|last| {
                last.message == entry.message
                    && last.level == entry.level
                    && last.operation_id == entry.operation_id
            }) {
                return;
            }

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
    fn consecutive_identical_messages_are_collapsed_to_one() {
        // 回归：轮询循环里每秒上报同一句 stage，会把日志区刷满。实测真实会话里
        // 「正在等待 fastbootd 设备」重复过 527 次，占整个日志的 67%。
        let path = std::env::temp_dir().join(format!(
            "nwflash-operation-log-dedupe-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let store = OperationLogStore::new(Some(path.clone()), 100);
        let op = Some("op-1".to_string());

        store.write(
            OperationLogLevel::Info,
            "正在等待设备".to_owned(),
            op.clone(),
        );
        for _ in 0..50 {
            store.write(
                OperationLogLevel::Info,
                "正在等待设备".to_owned(),
                op.clone(),
            );
        }
        store.write(OperationLogLevel::Info, "开始刷写".to_owned(), op.clone());

        let messages = store
            .snapshot()
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["正在等待设备", "开始刷写"]);

        // 磁盘历史同样只落一条重复项（与内存快照一致）。
        let persisted = fs::read_to_string(&path).expect("log should be readable");
        assert_eq!(
            persisted.matches("正在等待设备").count(),
            1,
            "重复项不得写进磁盘历史：{persisted}"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn dedupe_is_scoped_to_consecutive_entries_and_full_triple() {
        let path = std::env::temp_dir().join(format!(
            "nwflash-operation-log-dedupe-scope-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let store = OperationLogStore::new(Some(path.clone()), 100);

        // 同一文案被**别的日志**隔开时是真实事件，必须保留。
        store.write(OperationLogLevel::Info, "A".to_owned(), None);
        store.write(OperationLogLevel::Info, "B".to_owned(), None);
        store.write(OperationLogLevel::Info, "A".to_owned(), None);
        // 只有 message 相同的行仍要按级别区分。
        store.write(OperationLogLevel::Warning, "A".to_owned(), None);
        // 只有 message + 级别相同、但操作不同，也要区分（两次刷写各记各的）。
        store.write(
            OperationLogLevel::Warning,
            "A".to_owned(),
            Some("op-2".to_string()),
        );

        let messages = store
            .snapshot()
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>();
        assert_eq!(
            messages,
            vec!["A", "B", "A", "A", "A"],
            "被隔开 / 级别不同 / 操作不同的同一文案都不得被吞"
        );
        let _ = fs::remove_file(path);
    }

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
