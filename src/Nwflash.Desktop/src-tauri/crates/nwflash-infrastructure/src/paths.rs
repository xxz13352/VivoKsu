use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 默认资源根目录（兼容现有 WPF 约定）。
pub const PREFERRED_ROOT: &str = r"C:\nwflash";

/// 资源根目录优先写 `C:\nwflash`，不可写则回退到 `%LOCALAPPDATA%\VivoKsu`。
pub fn resource_root() -> PathBuf {
    let preferred = PathBuf::from(PREFERRED_ROOT);
    if try_make_writable(&preferred) {
        return preferred;
    }

    let fallback_base = env::var_os("LOCALAPPDATA").or_else(|| env::var_os("APPDATA"));
    let fallback = fallback_base
        .map(|value| PathBuf::from(value).join("VivoKsu"))
        .unwrap_or_else(|| env::temp_dir().join("VivoKsu"));

    if try_make_writable(&fallback) {
        fallback
    } else {
        env::temp_dir()
    }
}

/// 用于在测试/诊断时检测目录写入能力（建立目录 + 写 probe + 删除）。
pub fn try_make_writable(path: &Path) -> bool {
    write_test_probe(path).is_ok()
}

pub(crate) fn write_test_probe(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| format!("create directory failed: {error}"))?;
    let random = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("time unavailable: {error}"))?
        .as_nanos();
    let probe = path.join(format!(".write-{random}.tmp"));
    fs::write(&probe, b"ok").map_err(|error| format!("write probe failed: {error}"))?;
    match fs::remove_file(&probe) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("remove probe failed: {error}")),
    }
}
