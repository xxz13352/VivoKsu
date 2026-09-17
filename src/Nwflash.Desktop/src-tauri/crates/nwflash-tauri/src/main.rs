use std::{
    fs::{create_dir_all, OpenOptions},
    io::Write,
    path::PathBuf,
    panic,
    time::{SystemTime, UNIX_EPOCH},
};

fn crash_log_path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    let mut path = PathBuf::from(base);
    path.push("Nwflash");
    Some(path)
}

fn write_crash_log(payload: &str) {
    let Some(base_dir) = crash_log_path() else {
        return;
    };

    if create_dir_all(&base_dir).is_err() {
        return;
    }

    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(base_dir.join("crash.log"))
    {
        Ok(file) => file,
        Err(_) => return,
    };

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let _ = writeln!(file, "[{timestamp}] {payload}");
}

fn install_crash_hook() {
    let previous = panic::take_hook();

    panic::set_hook(Box::new(move |info| {
        write_crash_log(&format!("panic: {info}"));
        previous(info);
    }));
}

fn main() {
    install_crash_hook();
    if let Err(error) = nwflash_tauri::run_app(tauri::generate_context!("../../tauri.conf.json")) {
        panic!("nwflash module failed: {error}");
    }
}
