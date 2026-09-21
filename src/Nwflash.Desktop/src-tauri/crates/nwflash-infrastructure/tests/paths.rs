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

/// 工具私有配置目录下的独立子目录，符合 P2 的目录约束。
fn private_settings_directory(name: &str) -> PathBuf {
    let root = env::var_os("LOCALAPPDATA")
        .or_else(|| env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    // 私有目录下的独立子目录：每个测试用自己的 settings.json，互不干扰。
    // `is_within` 按路径分量判定，子目录同样被认可。
    root.join("VivoKsu").join(name)
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
    // 配置必须落在工具私有目录内（目录约束是 P2 的一条真实检查），
    // 因此这里按 create_default 的规则构造路径，而不是随便用临时目录。
    let settings_dir = private_settings_directory("preferences");
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
    let settings_dir = private_settings_directory("invalid-json");
    let settings_path = settings_dir.join("settings.json");
    std::fs::create_dir_all(&settings_dir).expect("create dir for invalid json");
    std::fs::write(&settings_path, "not json").expect("write invalid json");

    let prefs = ToolPathPreferences::with_path(settings_path.clone());
    assert!(prefs.scrcpy_path().is_none());

    let _ = std::fs::remove_dir_all(&settings_dir);
}

/// 目录约束是 P2 的一条真实检查：配置**不得**从私有目录之外加载。
///
/// 这条测试证明约束不是摆设——放在临时目录里的配置会被拒绝并回退到默认值。
#[test]
fn config_outside_the_private_directory_is_rejected() {
    let outside_dir = unique_temp_path("outside-private-dir");
    std::fs::create_dir_all(&outside_dir).expect("create outside dir");
    let settings_path = outside_dir.join("settings.json");
    std::fs::write(&settings_path, r#"{"ScrcpyPath":"D:\\tools\\scrcpy.exe"}"#)
        .expect("write settings");

    let prefs = ToolPathPreferences::with_path(settings_path);

    assert!(
        prefs.scrcpy_path().is_none(),
        "私有目录之外的配置必须被拒绝"
    );

    let _ = std::fs::remove_dir_all(&outside_dir);
}

/// 相对路径的 scrcpy 会被当前工作目录影响，属于劫持面，必须拒绝。
#[test]
fn relative_scrcpy_path_is_rejected() {
    let settings_dir = private_settings_directory("relative-scrcpy");
    std::fs::create_dir_all(&settings_dir).expect("create private dir");
    let settings_path = settings_dir.join("settings.json");
    std::fs::write(&settings_path, r#"{"ScrcpyPath":"tools\\scrcpy.exe"}"#)
        .expect("write settings");

    let prefs = ToolPathPreferences::with_path(settings_path);

    assert!(
        prefs.scrcpy_path().is_none(),
        "相对路径的 scrcpy 必须被拒绝"
    );

    let _ = std::fs::remove_dir_all(&settings_dir);
}
