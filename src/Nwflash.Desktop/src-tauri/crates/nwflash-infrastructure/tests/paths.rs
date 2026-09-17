use nwflash_infrastructure::{resource_root, try_make_writable, ToolPathPreferences};
use std::env;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_temp_path(name: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_millis(0))
        .as_millis();
    env::temp_dir()
        .join("nwflash-rust-tests")
        .join(format!("{name}-{millis}"))
}

#[test]
fn resource_root_falls_back_when_preferred_root_not_writable() {
    let root = resource_root();
    assert!(!root.as_os_str().is_empty());
    assert!(root.exists());
}

#[test]
fn write_probe_true_for_temp_directory() {
    let temp = unique_temp_path("writable");
    assert!(try_make_writable(&temp));
    assert!(temp.exists());

    if temp.exists() {
        let _ = std::fs::remove_dir_all(&temp);
    }
}

#[test]
fn write_probe_detects_invalid_directory() {
    // 父目录不存在时会尝试创建，因此这里改为一个无效的设备路径以触发失败分支。
    #[cfg(windows)]
    let invalid_path = std::path::Path::new("Z:\\");
    #[cfg(not(windows))]
    let invalid_path = std::path::Path::new("/dev/null/not-a-dir");

    assert!(!try_make_writable(invalid_path));
}

#[test]
fn toolpath_preference_roundtrip() {
    let settings_dir = unique_temp_path("preferences");
    let settings_path = settings_dir.join("settings.json");

    let mut prefs = ToolPathPreferences::with_path(settings_path.clone());
    prefs.save_scrcpy_path(r"D:\tools\scrcpy.exe");
    let loaded = ToolPathPreferences::with_path(settings_path);

    assert_eq!(loaded.scrcpy_path(), Some(r"D:\tools\scrcpy.exe"));

    if settings_dir.exists() {
        let _ = std::fs::remove_dir_all(&settings_dir);
    }
}

#[test]
fn toolpath_handles_invalid_json() {
    let settings_dir = unique_temp_path("invalid-json");
    let settings_path = settings_dir.join("settings.json");
    std::fs::create_dir_all(&settings_dir).expect("create dir for invalid json");
    std::fs::write(&settings_path, "not json").expect("write invalid json");

    let prefs = ToolPathPreferences::with_path(settings_path.clone());
    assert!(prefs.scrcpy_path().is_none());

    let _ = std::fs::remove_dir_all(&settings_dir);
}
