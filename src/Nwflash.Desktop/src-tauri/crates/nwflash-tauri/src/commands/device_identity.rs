//! 共享设备身份读取：从 ADB 设备读取 PD（产品代号）与系统版本。
//! 供安全刷写（线刷）与 Vivo ROOT 云端 OTA 提取共用。
//! 读设备信息在 Rust 内部完成，浏览器不提交 serial，也不获得原始输出。

use nwflash_windows::{
    device_transport::DeviceTransport,
    platform_tools::PlatformTools,
    process::{run_command, ProcessOutput},
};

pub async fn read_online_ota_identity(serial: &str) -> Result<(String, String), String> {
    let serial = serial.to_string();
    tokio::task::spawn_blocking(move || {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));
        let command = transport
            .build_adb_getprop_command(&serial)
            .map_err(|_| "无法构造设备信息读取请求。".to_string())?;
        let output =
            run_command(command).map_err(|_| "读取已连接设备的 PD/版本失败。".to_string())?;
        online_ota_identity_from_process_output(output)
    })
    .await
    .map_err(|_| "读取设备信息调度失败。".to_string())?
}

pub fn online_ota_identity_from_process_output(
    output: ProcessOutput,
) -> Result<(String, String), String> {
    if output.exit_code != 0 {
        return Err("读取已连接设备的 PD/版本失败。".to_string());
    }
    online_ota_identity_from_getprop(&output.stdout)
}

pub fn online_ota_identity_from_getprop(output: &str) -> Result<(String, String), String> {
    let value = |key: &str| {
        output.lines().find_map(|line| {
            line.trim()
                .strip_prefix(&format!("[{key}]: ["))
                .and_then(|value| value.strip_suffix(']'))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
    };
    let pd = value("ro.product.device").ok_or_else(|| "无法从已连接设备读取 PD。".to_string())?;
    let version = value("ro.build.version.bbk")
        .and_then(|bbk| bbk.rsplit('_').next())
        .or_else(|| value("ro.build.display.id"))
        .or_else(|| value("ro.build.version.incremental"))
        .or_else(|| value("ro.vivo.os.build.display.id"))
        .ok_or_else(|| "无法从已连接设备读取系统版本。".to_string())?;
    Ok((pd.to_string(), version.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nwflash_windows::process::ProcessOutput;

    #[test]
    fn getprop_parses_pd_and_bbk_version_tail() {
        let output = "\n\
            [ro.product.device]: [PD2417]\n\
            [ro.build.version.bbk]: [DPD2221B_A_16.2.12.0.W10.V000L1]\n\
            [ro.build.display.id]: [PD2417A_16.2.12]\n";
        let (pd, version) = online_ota_identity_from_getprop(output)
            .expect("完整 bbk 行应解析出 PD 与版本末段");

        assert_eq!(pd, "PD2417");
        assert_eq!(version, "16.2.12.0.W10.V000L1");
    }

    #[test]
    fn device_identity_failure_does_not_expose_adb_output() {
        let output = ProcessOutput {
            exit_code: 1,
            stdout: "SERIAL-SECRET".to_string(),
            stderr: "adb -s SERIAL-SECRET token=private https://rom.invalid/ota.zip".to_string(),
        };

        let error = online_ota_identity_from_process_output(output)
            .expect_err("failed getprop must return a safe categorized error");

        assert_eq!(error, "读取已连接设备的 PD/版本失败。");
        assert!(!error.contains("SERIAL-SECRET"));
        assert!(!error.contains("private"));
        assert!(!error.contains("rom.invalid"));
        assert!(!error.contains("adb"));
    }
}
