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
/// 启动时从磁盘读入内存的**行数**上限（界面历史窗口）。
///
/// 与 `MAX_LOG_FILE_BYTES`（写盘的字节阈值）不是一回事：那一层只保证文件不
/// 超过 2 MiB，而单条日志可能很长（例如上万字符的分区清单），2 MiB 完全可能
/// 装着远超 `DEFAULT_MAX_ENTRIES` 的行数。
const MAX_LOG_LINES_ON_LOAD: usize = 4096;

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
            .and_then(|path| read_entries_from_file(path).ok())
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

fn read_entries_from_file(path: &Path) -> std::io::Result<Vec<OperationLogEntry>> {
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

    Ok(trim_loaded_entries(entries))
}

/// 把读入的历史裁到 `MAX_LOG_LINES_ON_LOAD` 行以内。
///
/// 只要窗口内存在合法起点就从**操作边界**起裁：逐分区流水只是一次操作内部的
/// 重复行，按行数硬裁会把同一次刷写切成半截——实测 `operations.log` 停在 500
/// 行时，历史窗口的第一条是「正在等待 fastbootd 设备」（某个卡了 9 分钟、写了
/// 527 行的操作的中段），而文件里真正的第一条是「线刷测试」。按操作边界裁，
/// 窗口要么完整包含这次操作，要么整段不含，不会展示半截流水。
///
/// `operation_id` 为 `None` 的记录（保护拒绝等审计条目）没有可依赖的边界，
/// 按出现顺序与相邻记录一起保留。
fn trim_loaded_entries(entries: Vec<OperationLogEntry>) -> Vec<OperationLogEntry> {
    if entries.len() <= MAX_LOG_LINES_ON_LOAD {
        return entries;
    }

    let min_keep = entries.len() - MAX_LOG_LINES_ON_LOAD;
    // 从 `min_keep` 往后找第一个「操作边界」：该位置的记录开启了它所属的操作
    // （自身或前一条没有 operation_id），从这里裁不会留下半截操作。
    let cut =
        (min_keep..entries.len()).find(|&index| match entries[index].operation_id.as_deref() {
            None => true,
            Some(id) => match entries.get(index.wrapping_sub(1)) {
                Some(previous) => previous.operation_id.as_deref() != Some(id),
                None => true,
            },
        });

    let keep_from = match cut {
        Some(cut) if cut > 0 => cut,
        // 窗口内找不到合法起点（整个候选区都是同一个操作的中段）：退回按行数
        // 裁，**不能**因为「不想截断」就突破行数上限。
        _ => min_keep,
    };
    entries[keep_from..].to_vec()
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

    fn entry(
        level: OperationLogLevel,
        message: &str,
        operation_id: Option<&str>,
    ) -> OperationLogEntry {
        OperationLogEntry {
            timestamp_utc: 0,
            level,
            message: message.to_owned(),
            operation_id: operation_id.map(str::to_owned),
        }
    }

    /// 构造一段「`mid_rows` 行中段 + 结尾一条完整操作」的历史，行长固定，便于让
    /// 硬裁点精确落在中段行上。
    fn history_with_mid_stream_operation(
        mid_rows: usize,
        id: &str,
        last_message: &str,
    ) -> Vec<OperationLogEntry> {
        let mut entries = vec![entry(OperationLogLevel::Info, "线刷测试", None)];
        entries.push(entry(
            OperationLogLevel::Info,
            &format!("{id} 起点"),
            Some(id),
        ));
        for _ in 0..mid_rows {
            entries.push(entry(
                OperationLogLevel::Info,
                "正在等待 fastbootd 设备",
                Some(id),
            ));
        }
        entries.push(entry(OperationLogLevel::Success, last_message, Some(id)));
        // 紧随其后的一次短操作：它是窗口边界之后唯一的合法起点。
        entries.push(entry(
            OperationLogLevel::Info,
            "检查快速刷写条件",
            Some("op-next"),
        ));
        entries.push(entry(
            OperationLogLevel::Success,
            "快速刷写完成",
            Some("op-next"),
        ));
        entries
    }

    #[test]
    fn loading_caps_at_the_line_limit_and_never_opens_mid_operation() {
        // 回归：历史文件可以远超界面窗口（实测 1563 行，仍远低于 2 MiB 轮转阈值）。
        // 旧实现按调用方的 500 行硬裁，裁点正好落在一次超长操作的中段，界面历史的
        // 第一条成了「正在等待 fastbootd 设备」这种流水。这里刻意让硬裁点
        // （len - 500）落在 5003 行的超长操作内部，且该点之后唯一的合法起点属于
        // 紧随其后的短操作。总长 5005 > 4096，所以软裁点（5005 - 4096 = 909）也在
        // 同一个超长操作内部。
        let entries = history_with_mid_stream_operation(5000, "op-stuck", "线刷完成");

        let trimmed = trim_loaded_entries(entries);

        // 窗口不超过上限。
        assert!(trimmed.len() <= MAX_LOG_LINES_ON_LOAD);
        // 不能在操作中间开窗：窗口第一行必须是一次操作的起点。硬裁点（len - 500）
        // 落在 3003 行操作的中段，所以这里只有「从边界起裁」才可能过。
        assert_eq!(trimmed[0].message, "检查快速刷写条件");
        assert_eq!(trimmed[0].operation_id.as_deref(), Some("op-next"));
        assert_eq!(trimmed.last().unwrap().message, "快速刷写完成");
    }

    #[test]
    fn loading_keeps_everything_under_the_line_limit() {
        let entries = vec![
            entry(OperationLogLevel::Info, "第一条", None),
            entry(OperationLogLevel::Info, "第二条", Some("op-1")),
        ];
        let trimmed = trim_loaded_entries(entries.clone());
        assert_eq!(trimmed.len(), entries.len());
        assert_eq!(trimmed[0].message, "第一条");
        assert_eq!(trimmed[1].message, "第二条");
    }

    #[test]
    fn loading_falls_back_to_a_line_cut_when_the_window_has_no_operation_start() {
        // 兜底：整个窗口候选区都是一个操作的中段，找不到合法起点，只能按行数裁，
        // 且必须仍然满足上限。
        let mut entries = Vec::new();
        for _ in 0..5000 {
            entries.push(entry(OperationLogLevel::Info, "op-a 中段", Some("op-a")));
        }

        let trimmed = trim_loaded_entries(entries);
        assert_eq!(trimmed.len(), MAX_LOG_LINES_ON_LOAD);
        assert!(trimmed
            .iter()
            .all(|e| e.operation_id.as_deref() == Some("op-a")));
    }

    #[test]
    fn loading_treats_a_record_without_id_as_an_operation_boundary() {
        // `None` 记录不构成需要保护的边界：它是匿名历史尾巴的起点，从这里裁合法。
        let mut entries = vec![entry(OperationLogLevel::Info, "线刷测试", None)];
        for _ in 0..2000 {
            entries.push(entry(OperationLogLevel::Info, "op-a 中段", Some("op-a")));
        }
        entries.push(entry(
            OperationLogLevel::Warning,
            "服务端未许可此操作",
            None,
        ));
        for _ in 0..3000 {
            entries.push(entry(OperationLogLevel::Info, "匿名检索流水", None));
        }

        let trimmed = trim_loaded_entries(entries);
        assert!(trimmed.len() <= MAX_LOG_LINES_ON_LOAD);
        assert!(trimmed.iter().all(|e| e.operation_id.is_none()));
        // 窗口内必须留下边界后的完整匿名段，而不是 op-a 的中段。
        assert!(trimmed.first().unwrap().message == "服务端未许可此操作");
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
