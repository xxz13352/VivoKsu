use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use nwflash_application::result_to_domain_error;
use nwflash_domain::DomainError;
use nwflash_windows::{
    detect_drivers, driver_install_failure_detail, driver_install_succeeded,
    locate_bundled_driver_archive, DriverDetectionPaths, DriverInstaller,
};
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct DriverReinstallDto {
    pub exit_code: i32,
    pub adb_driver_installed: bool,
    pub fastboot_driver_installed: bool,
    pub mediatek_driver_installed: bool,
}

#[tauri::command]
pub async fn driver_reinstall(
    state: State<'_, crate::AppState>,
) -> Result<DriverReinstallDto, String> {
    // 写类命令入口守卫：驱动安装会在本机产生提权副作用（写入系统驱动目录）。
    crate::commands::guard::guard_write_command(&state)?;
    let archive = locate_bundled_driver_archive(&nwflash_windows::bundled_resource_root())
        .ok_or_else(|| "未找到随附的 USB 驱动包，请重新安装奶蛙Flash。".to_string())?;
    let adb_usb_ini = default_adb_usb_ini_path()?;
    let exit_code = Arc::new(Mutex::new(None));
    let completed_exit_code = Arc::clone(&exit_code);
    let exit_code_for_operation = Arc::clone(&exit_code);

    state
        .operation_coordinator
        .run_shared_async(
            nwflash_domain::OperationKind::Installing,
            "安装 USB 驱动",
            move |context, cancellation| async move {
                context.report_stage("解压 USB 驱动包");
                let outcome = tauri::async_runtime::spawn_blocking(move || {
                    DriverInstaller::new(archive, adb_usb_ini)
                        .install_with_cancel_detailed(|| cancellation.is_cancelled())
                })
                .await
                .map_err(|error| DomainError::Internal(format!("驱动安装任务异常：{error}")))??;

                if !driver_install_succeeded(&outcome) {
                    // 保留 pnputil 的原始输出：退出码 1 可能是用法错误、签名失败
                    // 或某个 INF 被拒，只说“退出码 1”用户和我们都无法定位。
                    // 也可能退出码是 0/5 但输出里有 Failed to add driver package
                    // （实测个别 INF 失败时整体退出码仍为 5），所以判定要走
                    // driver_install_succeeded 而不是只看 exit_code。
                    return Err(DomainError::ExternalTool(driver_install_failure_detail(
                        &outcome,
                    )));
                }

                *exit_code_for_operation
                    .lock()
                    .expect("driver install result lock should not be poisoned") =
                    Some(outcome.exit_code);
                context.report_stage("USB 驱动安装完成");
                context.report_progress(1.0);
                Ok(())
            },
        )
        .await
        .map_err(|error| result_to_domain_error(error).to_string())?;

    let drivers = detect_drivers(&DriverDetectionPaths::default_windows());
    let completed_exit_code = (*completed_exit_code
        .lock()
        .expect("driver install result lock should not be poisoned"))
    .unwrap_or(0);
    Ok(DriverReinstallDto {
        exit_code: completed_exit_code,
        adb_driver_installed: drivers.adb_installed,
        fastboot_driver_installed: drivers.fastboot_installed,
        mediatek_driver_installed: drivers.mediatek_installed,
    })
}

fn default_adb_usb_ini_path() -> Result<PathBuf, String> {
    let profile = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .ok_or_else(|| "无法定位当前用户目录，不能写入 adb_usb.ini。".to_string())?;
    Ok(default_adb_usb_ini_path_from_profile(&profile))
}

pub(crate) fn default_adb_usb_ini_path_from_profile(profile: &Path) -> PathBuf {
    profile.join(".android").join("adb_usb.ini")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::default_adb_usb_ini_path_from_profile;

    #[test]
    fn default_adb_usb_ini_path_stays_in_the_current_users_android_directory() {
        assert_eq!(
            default_adb_usb_ini_path_from_profile(Path::new(r"C:\Users\Tester")),
            Path::new(r"C:\Users\Tester\.android\adb_usb.ini"),
        );
    }
}
