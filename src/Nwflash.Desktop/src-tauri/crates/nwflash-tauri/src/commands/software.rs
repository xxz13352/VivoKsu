use std::{fs, path::Path};

use nwflash_infrastructure::{PayloadDumperProvisioner, ScrcpyProvisioner, DEFAULT_APP_VERSION};
use nwflash_windows::{detect_drivers, DriverDetectionPaths, DriverStatus};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SoftwareStatusDto {
    pub app_version: String,
    pub adb_ready: bool,
    pub fastboot_ready: bool,
    pub scrcpy_ready: bool,
    pub payload_dumper_ready: bool,
    pub adb_driver_installed: bool,
    pub fastboot_driver_installed: bool,
    pub mediatek_driver_installed: bool,
}

#[tauri::command]
pub fn software_status() -> SoftwareStatusDto {
    // Platform tools ship as Tauri bundle resources (`resources/platform-tools/*`
    // in tauri.conf.json), so they land under `<exe dir>/resources`, not next to
    // the executable itself.
    let app_root = nwflash_windows::bundled_resource_root();
    let drivers = detect_drivers(&DriverDetectionPaths::default_windows());
    let scrcpy_ready = ScrcpyProvisioner::bundled(app_root.clone()).is_installed();
    let payload_dumper_ready = PayloadDumperProvisioner::bundled(app_root.clone()).is_available();

    software_status_from_app_root(&app_root, drivers, scrcpy_ready, payload_dumper_ready)
}

fn software_status_from_app_root(
    app_root: &Path,
    drivers: DriverStatus,
    scrcpy_ready: bool,
    payload_dumper_ready: bool,
) -> SoftwareStatusDto {
    let platform_tools = app_root.join("platform-tools");
    SoftwareStatusDto {
        app_version: DEFAULT_APP_VERSION.to_string(),
        adb_ready: is_non_empty_file(&platform_tools.join("adb.exe")),
        fastboot_ready: is_non_empty_file(&platform_tools.join("fastboot.exe")),
        scrcpy_ready,
        payload_dumper_ready,
        adb_driver_installed: drivers.adb_installed,
        fastboot_driver_installed: drivers.fastboot_installed,
        mediatek_driver_installed: drivers.mediatek_installed,
    }
}

fn is_non_empty_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use nwflash_infrastructure::DEFAULT_APP_VERSION;
    use nwflash_windows::DriverStatus;

    fn temporary_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("nwflash-software-status-{nonce}"));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    #[test]
    fn status_derives_platform_tool_readiness_without_returning_local_paths() {
        let root = temporary_directory();
        let tools = root.join("platform-tools");
        fs::create_dir_all(&tools).expect("platform tools directory should be created");
        fs::write(tools.join("adb.exe"), b"adb").expect("adb fixture should be written");
        fs::write(tools.join("fastboot.exe"), b"fastboot")
            .expect("fastboot fixture should be written");

        let status = software_status_from_app_root(
            &root,
            DriverStatus {
                adb_installed: true,
                fastboot_installed: false,
                mediatek_installed: true,
            },
            true,
            false,
        );

        assert!(status.adb_ready);
        assert!(status.fastboot_ready);
        assert!(status.scrcpy_ready);
        assert!(!status.payload_dumper_ready);
        assert!(status.adb_driver_installed);
        assert!(!status.fastboot_driver_installed);
        assert!(status.mediatek_driver_installed);
        let serialized = serde_json::to_value(status).expect("status should serialize");
        assert_eq!(
            serialized
                .get("app_version")
                .and_then(serde_json::Value::as_str),
            Some(DEFAULT_APP_VERSION),
        );
        fs::remove_dir_all(root).expect("temporary directory should be removed");
    }

    #[test]
    fn software_status_uses_the_current_wpf_release_version() {
        assert_eq!(DEFAULT_APP_VERSION, "1.0.1");
    }
}
