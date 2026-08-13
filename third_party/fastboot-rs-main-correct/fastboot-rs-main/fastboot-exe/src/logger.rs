// Lightweight file logger: appends the tool's key output (status / success /
// error / flash lifecycle / auth events) to a log directory on the G: drive
// so the operator can review what happened afterwards.
//
// Notes (kept ASCII-only on purpose so the source is always valid UTF-8;
// the actual log lines may contain UTF-8 Chinese forwarded from callers):
// - Directory priority: env RMFLASH_LOG_DIR > G:\RMFlashLog >
//   %USERPROFILE%\RMFlashLog > system temp dir. First creatable wins; if all
//   fail, logging silently turns into a no-op and never affects the main flow.
// - One file per day: fastboot-YYYYMMDD.log, appended across invocations.
// - Thread-safe (Mutex); all IO errors are ignored, never panics.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_FILE: OnceLock<Option<Mutex<fs::File>>> = OnceLock::new();

/// Returns (date string for the filename, full local timestamp string).
#[cfg(windows)]
fn now_string() -> (String, String) {
    use winapi::um::minwinbase::SYSTEMTIME;
    use winapi::um::sysinfoapi::GetLocalTime;
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe {
        GetLocalTime(&mut st);
    }
    let date = format!("{:04}{:02}{:02}", st.wYear, st.wMonth, st.wDay);
    let stamp = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
    );
    (date, stamp)
}

#[cfg(not(windows))]
fn now_string() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let day = secs / 86_400;
    (format!("day{}", day), format!("epoch+{}s", secs))
}

fn resolve_log_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("RMFLASH_LOG_DIR") {
        let t = d.trim();
        if !t.is_empty() {
            candidates.push(PathBuf::from(t));
        }
    }
    candidates.push(PathBuf::from(r"G:\RMFlashLog"));
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("RMFlashLog"));
    }
    candidates.push(std::env::temp_dir().join("RMFlashLog"));

    for dir in candidates {
        if fs::create_dir_all(&dir).is_ok() {
            return Some(dir);
        }
    }
    None
}

fn open_log() -> Option<Mutex<fs::File>> {
    let dir = resolve_log_dir()?;
    let (date, _) = now_string();
    let path = dir.join(format!("fastboot-{}.log", date));
    let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    Some(Mutex::new(file))
}

fn handle() -> Option<&'static Mutex<fs::File>> {
    LOG_FILE.get_or_init(open_log).as_ref()
}

/// Append one timestamped line to the log (errors are silently ignored).
pub fn log_line(msg: &str) {
    if let Some(m) = handle() {
        if let Ok(mut f) = m.lock() {
            let (_, ts) = now_string();
            let _ = writeln!(f, "[{}] {}", ts, msg);
            let _ = f.flush();
        }
    }
}

/// Record the start of one process invocation (command line + version).
pub fn log_invocation() {
    let argv: Vec<String> = std::env::args().collect();
    let cmdline = argv.join(" ");
    log_line(&format!(
        "==== fastboot v{} | {} ====",
        env!("CARGO_PKG_VERSION"),
        cmdline
    ));
}

/// The log directory actually in use (handy for telling the user where logs go).
pub fn current_log_dir() -> Option<PathBuf> {
    resolve_log_dir()
}
