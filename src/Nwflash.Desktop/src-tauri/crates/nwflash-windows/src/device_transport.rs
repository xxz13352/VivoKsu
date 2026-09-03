//! Device transport command builders for fastboot / adb-root usage.

use nwflash_domain::DomainError;

use crate::{platform_tools::PlatformTools, process::ProcessCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceTransport {
    platform_tools: PlatformTools,
}

impl DeviceTransport {
    pub fn new(platform_tools: PlatformTools) -> Self {
        Self { platform_tools }
    }

    pub fn build_fastboot_flash_command(
        &self,
        serial: &str,
        partition: &str,
        image_path: &str,
    ) -> Result<ProcessCommand, DomainError> {
        let arguments = Self::build_transport_args(&[
            "flash".to_string(),
            partition.to_string(),
            image_path.to_string(),
        ])?;
        self.platform_tools.fastboot_command(serial, &arguments)
    }

    pub fn build_fastboot_erase_command(
        &self,
        serial: &str,
        partition: &str,
    ) -> Result<ProcessCommand, DomainError> {
        let arguments = Self::build_transport_args(&["erase".to_string(), partition.to_string()])?;
        self.platform_tools.fastboot_command(serial, &arguments)
    }

    pub fn build_adb_root_copy_from_device_command(
        &self,
        serial: &str,
        device_path: &str,
        local_path: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        validate_device_path(device_path)?;
        validate_local_path(local_path)?;

        let arguments = vec![
            "exec-out".to_string(),
            "su".to_string(),
            // APatch allocates a PTY by default.  PTY output translates LF
            // bytes to CRLF, corrupting binary partition backups.
            "--no-pty".to_string(),
            "-c".to_string(),
            "dd".to_string(),
            format!("if={device_path}"),
            "bs=4M".to_string(),
            "2>/dev/null".to_string(),
        ];
        self.platform_tools.adb_command(serial, &arguments)
    }

    pub fn build_adb_root_copy_to_device_command(
        &self,
        serial: &str,
        local_path: &str,
        device_path: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        validate_device_path(device_path)?;
        validate_local_path(local_path)?;

        Err(DomainError::InvalidOperation(
            "ADB Root 二进制写入必须使用暂存上传流程。".to_string(),
        ))
    }

    pub fn build_adb_push_to_staging_command(
        &self,
        serial: &str,
        local_path: &str,
        staging_path: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        validate_local_path(local_path)?;
        validate_adb_staging_path(staging_path)?;

        self.platform_tools.adb_command(
            serial,
            &[
                "push".to_string(),
                local_path.to_string(),
                staging_path.to_string(),
            ],
        )
    }

    pub fn build_adb_root_copy_staged_file_to_device_command(
        &self,
        serial: &str,
        staging_path: &str,
        device_path: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        validate_adb_staging_path(staging_path)?;
        validate_device_path(device_path)?;

        let command = format!(
            "dd if={} of={} bs=4M conv=fsync",
            shell_quote(staging_path),
            shell_quote(device_path)
        );
        self.platform_tools.adb_command(
            serial,
            &[
                "shell".to_string(),
                "-T".to_string(),
                "su".to_string(),
                "--no-pty".to_string(),
                "-c".to_string(),
                shell_quote(&command),
            ],
        )
    }

    pub fn build_adb_remove_staging_command(
        &self,
        serial: &str,
        staging_path: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        validate_adb_staging_path(staging_path)?;

        self.platform_tools.adb_command(
            serial,
            &[
                "shell".to_string(),
                "-T".to_string(),
                "rm".to_string(),
                "-f".to_string(),
                "--".to_string(),
                staging_path.to_string(),
            ],
        )
    }

    pub fn build_adb_root_shell_command(
        &self,
        serial: &str,
        command: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        if command.trim().is_empty() {
            return Err(DomainError::InvalidInput(
                "Root shell 命令不能为空。".to_string(),
            ));
        }

        let arguments = vec![
            "shell".to_string(),
            "-T".to_string(),
            "su".to_string(),
            "--no-pty".to_string(),
            "-c".to_string(),
            shell_quote(command),
        ];
        self.platform_tools.adb_command(serial, &arguments)
    }

    pub fn build_adb_reboot_fastboot_command(
        &self,
        serial: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        self.platform_tools
            .adb_command(serial, &["reboot".to_string(), "fastboot".to_string()])
    }

    pub fn build_adb_reboot_system_command(
        &self,
        serial: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        self.platform_tools
            .adb_command(serial, &["reboot".to_string()])
    }

    pub fn build_adb_reboot_bootloader_command(
        &self,
        serial: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        self.platform_tools
            .adb_command(serial, &["reboot".to_string(), "bootloader".to_string()])
    }

    pub fn build_adb_getprop_command(&self, serial: &str) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        self.platform_tools
            .adb_command(serial, &["shell".to_string(), "getprop".to_string()])
    }

    pub fn build_adb_battery_command(&self, serial: &str) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        self.platform_tools.adb_command(
            serial,
            &[
                "shell".to_string(),
                "dumpsys".to_string(),
                "battery".to_string(),
            ],
        )
    }

    pub fn build_fastboot_reboot_command(
        &self,
        serial: &str,
    ) -> Result<ProcessCommand, DomainError> {
        self.build_fastboot_reboot_target_command(serial, None)
    }

    /// Builds `fastboot -s <serial> reboot [target]` for devices that are
    /// already in bootloader/fastbootd mode.  The target is intentionally
    /// constrained by the caller's typed reboot target rather than accepting
    /// arbitrary frontend text.
    pub fn build_fastboot_reboot_target_command(
        &self,
        serial: &str,
        target: Option<&str>,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        let mut arguments = vec!["reboot".to_string()];
        if let Some(target) = target.filter(|value| !value.is_empty()) {
            arguments.push(target.to_string());
        }
        self.platform_tools.fastboot_command(serial, &arguments)
    }

    pub fn build_adb_root_erase_command(
        &self,
        serial: &str,
        device_path: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        validate_device_path(device_path)?;

        // Same reasoning as `build_adb_root_copy_to_device_command`: the
        // `|| dd if=/dev/zero …` fallback must stay one shell command.
        let command = format!(
            "blkdiscard {} || dd if=/dev/zero of={} bs=4M conv=fsync",
            shell_quote(device_path),
            shell_quote(device_path)
        );
        let arguments = vec![
            "shell".to_string(),
            "-T".to_string(),
            "su".to_string(),
            "--no-pty".to_string(),
            "-c".to_string(),
            shell_quote(&command),
        ];
        self.platform_tools.adb_command(serial, &arguments)
    }
    fn build_transport_args(arguments: &[String]) -> Result<Vec<String>, DomainError> {
        if arguments.is_empty() {
            return Err(DomainError::InvalidInput("参数不能为空。".to_string()));
        }
        for argument in arguments {
            if argument.trim().is_empty() {
                return Err(DomainError::InvalidInput("参数不能为空。".to_string()));
            }
        }

        Ok(arguments.to_vec())
    }

    pub fn build_fastboot_getvar_command(
        &self,
        serial: &str,
        variable: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        if variable.trim().is_empty() {
            return Err(DomainError::InvalidInput(
                "getvar 变量不能为空。".to_string(),
            ));
        }

        let arguments = vec!["getvar".to_string(), variable.trim().to_string()];
        self.platform_tools.fastboot_command(serial, &arguments)
    }

    pub fn build_fastboot_set_active_command(
        &self,
        serial: &str,
        slot: &str,
    ) -> Result<ProcessCommand, DomainError> {
        validate_serial(serial)?;
        if slot.trim().is_empty() {
            return Err(DomainError::InvalidInput("槽位不能为空。".to_string()));
        }

        let arguments = vec!["set_active".to_string(), slot.trim().to_string()];
        self.platform_tools.fastboot_command(serial, &arguments)
    }
}

fn validate_serial(serial: &str) -> Result<(), DomainError> {
    if serial.trim().is_empty() {
        return Err(DomainError::InvalidInput("设备串口不能为空。".to_string()));
    }
    Ok(())
}

fn validate_local_path(local_path: &str) -> Result<(), DomainError> {
    if local_path.trim().is_empty() {
        return Err(DomainError::InvalidInput("本地路径不能为空。".to_string()));
    }
    Ok(())
}

fn validate_device_path(device_path: &str) -> Result<(), DomainError> {
    if device_path.trim().is_empty() {
        return Err(DomainError::InvalidInput("设备路径不能为空。".to_string()));
    }
    if !device_path.starts_with("/dev/block/") {
        return Err(DomainError::InvalidInput(
            "设备路径必须以 /dev/block/ 开头。".to_string(),
        ));
    }
    if !device_path.chars().all(is_device_path_char) {
        return Err(DomainError::InvalidOperation(
            "设备路径包含非法字符。".to_string(),
        ));
    }
    // `.`/`..` and empty components would let a device-controlled path
    // escape /dev/block/ (e.g. `/dev/block/../sda` targets the whole disk),
    // and root dd/blkdiscard commands run with whatever path survives here.
    if device_path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(DomainError::InvalidOperation(
            "设备路径包含非法路径组件。".to_string(),
        ));
    }
    Ok(())
}

fn validate_adb_staging_path(staging_path: &str) -> Result<(), DomainError> {
    const PREFIX: &str = "/data/local/tmp/nwflash-stage-";

    if !staging_path.starts_with(PREFIX)
        || !staging_path.ends_with(".img")
        || staging_path.len() <= PREFIX.len() + ".img".len()
        || !staging_path.chars().all(is_device_path_char)
    {
        return Err(DomainError::InvalidInput(
            "ADB Root 暂存路径无效。".to_string(),
        ));
    }
    Ok(())
}

fn is_device_path_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '/' || ch == '.' || ch == '_' || ch == '-'
}

/// Wraps a value in single quotes for the device shell, escaping any embedded
/// single quote as `'"'"'`.  Mirrors the WPF `ShellQuote`.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_transport_validates_empty_serial() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        let err = transport
            .build_fastboot_flash_command("", "boot", "C:\\temp\\boot.img")
            .expect_err("serial required");
        assert!(err.to_string().contains("串口不能为空"));
    }

    #[test]
    fn device_transport_validates_device_path() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        let err = transport
            .build_adb_root_copy_from_device_command(
                "ABC123",
                "../../etc/passwd",
                "C:\\tmp\\out.bin",
            )
            .expect_err("bad device path rejected");
        assert!(err.to_string().contains("设备路径必须以 /dev/block/ 开头"));
    }

    #[test]
    fn device_transport_rejects_dot_components_under_dev_block() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));
        let err = transport
            .build_adb_root_erase_command("ABC123", "/dev/block/../sda")
            .expect_err("dot components must not escape /dev/block/");
        assert!(err.to_string().contains("设备路径包含非法路径组件"));
        let err = transport
            .build_adb_root_copy_staged_file_to_device_command(
                "ABC123",
                "/data/local/tmp/nwflash-stage-1-0.img",
                "/dev/block/./boot_a",
            )
            .expect_err("dot components must be rejected");
        assert!(err.to_string().contains("设备路径包含非法路径组件"));
    }

    #[test]
    fn device_transport_builds_fastboot_erase_args() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));
        let command = transport
            .build_fastboot_erase_command("ABC123", "userdata")
            .expect("command should build");
        assert_eq!(
            command.args,
            vec![
                "-s".to_string(),
                "ABC123".to_string(),
                "erase".to_string(),
                "userdata".to_string()
            ]
        );
    }

    #[test]
    fn adb_root_write_uploads_to_staging_before_root_dd_without_a_local_path_argument() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        let upload = transport
            .build_adb_push_to_staging_command(
                "ABC123",
                r"C:\images\boot.img",
                "/data/local/tmp/nwflash-stage-123.img",
            )
            .expect("a checked ADB staging upload command should build");
        let write = transport
            .build_adb_root_copy_staged_file_to_device_command(
                "ABC123",
                "/data/local/tmp/nwflash-stage-123.img",
                "/dev/block/sda12",
            )
            .expect("a checked ADB Root write command should build");

        assert_eq!(
            upload.args,
            vec![
                "-s",
                "ABC123",
                "push",
                r"C:\images\boot.img",
                "/data/local/tmp/nwflash-stage-123.img",
            ]
        );
        assert_eq!(
            write.args,
            vec![
                "-s",
                "ABC123",
                "shell",
                "-T",
                "su",
                "--no-pty",
                "-c",
                r#"'dd if='"'"'/data/local/tmp/nwflash-stage-123.img'"'"' of='"'"'/dev/block/sda12'"'"' bs=4M conv=fsync'"#,
            ]
        );
        assert!(!write
            .args
            .iter()
            .any(|argument| argument.contains("boot.img")));

        let cleanup = transport
            .build_adb_remove_staging_command("ABC123", "/data/local/tmp/nwflash-stage-123.img")
            .expect("a checked ADB staging cleanup command should build");
        assert_eq!(
            cleanup.args,
            vec![
                "-s",
                "ABC123",
                "shell",
                "-T",
                "rm",
                "-f",
                "--",
                "/data/local/tmp/nwflash-stage-123.img",
            ]
        );
    }

    #[test]
    fn adb_root_shell_quotes_the_complete_command_as_one_non_pty_argument() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        let command = transport
            .build_adb_root_shell_command(
                "ABC123",
                "for d in /dev/block/by-name; do [ -d \"$d\" ] || continue; done",
            )
            .expect("a checked ADB Root shell command should build");

        assert_eq!(
            command.args,
            vec![
                "-s",
                "ABC123",
                "shell",
                "-T",
                "su",
                "--no-pty",
                "-c",
                r#"'for d in /dev/block/by-name; do [ -d "$d" ] || continue; done'"#,
            ]
        );
    }

    #[test]
    fn adb_root_erase_quotes_the_blkdiscard_and_dd_fallback_command() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        let command = transport
            .build_adb_root_erase_command("ABC123", "/dev/block/sda12")
            .expect("a checked ADB Root erase command should build");

        assert_eq!(
            command.args,
            vec![
                "-s",
                "ABC123",
                "shell",
                "-T",
                "su",
                "--no-pty",
                "-c",
                r#"'blkdiscard '"'"'/dev/block/sda12'"'"' || dd if=/dev/zero of='"'"'/dev/block/sda12'"'"' bs=4M conv=fsync'"#,
            ]
        );
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("/dev/block/sda12"), "'/dev/block/sda12'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn adb_root_backup_uses_exec_out_without_a_local_path_argument() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        let command = transport
            .build_adb_root_copy_from_device_command(
                "ABC123",
                "/dev/block/sda12",
                r"C:\backups\boot.img",
            )
            .expect("a checked ADB Root backup command should build");

        assert_eq!(
            command.args,
            vec![
                "-s",
                "ABC123",
                "exec-out",
                "su",
                "--no-pty",
                "-c",
                "dd",
                "if=/dev/block/sda12",
                "bs=4M",
                "2>/dev/null",
            ]
        );
        assert!(!command
            .args
            .iter()
            .any(|argument| argument.contains("boot.img")));
    }

    #[test]
    fn device_transport_builds_fastboot_getvar_command() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));
        let command = transport
            .build_fastboot_getvar_command("ABC123", "current-slot")
            .expect("command should build");
        assert_eq!(
            command.args,
            vec![
                "-s".to_string(),
                "ABC123".to_string(),
                "getvar".to_string(),
                "current-slot".to_string()
            ]
        );
    }

    #[test]
    fn device_transport_builds_fixed_adb_device_info_commands() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        let properties = transport
            .build_adb_getprop_command("ABC123")
            .expect("getprop command should build");
        let battery = transport
            .build_adb_battery_command("ABC123")
            .expect("battery command should build");

        assert_eq!(properties.args, vec!["-s", "ABC123", "shell", "getprop"]);
        assert_eq!(
            battery.args,
            vec!["-s", "ABC123", "shell", "dumpsys", "battery"]
        );
    }

    #[test]
    fn device_transport_builds_only_supported_overview_reboot_targets() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));

        assert_eq!(
            transport
                .build_adb_reboot_system_command("ABC123")
                .unwrap()
                .args,
            vec!["-s", "ABC123", "reboot"]
        );
        assert_eq!(
            transport
                .build_adb_reboot_bootloader_command("ABC123")
                .unwrap()
                .args,
            vec!["-s", "ABC123", "reboot", "bootloader"]
        );
        assert_eq!(
            transport
                .build_adb_reboot_fastboot_command("ABC123")
                .unwrap()
                .args,
            vec!["-s", "ABC123", "reboot", "fastboot"]
        );
    }

    #[test]
    fn device_transport_builds_fastboot_set_active_command() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));
        let command = transport
            .build_fastboot_set_active_command("ABC123", "b")
            .expect("command should build");
        assert_eq!(
            command.args,
            vec![
                "-s".to_string(),
                "ABC123".to_string(),
                "set_active".to_string(),
                "b".to_string()
            ]
        );
    }

    #[test]
    fn device_transport_builds_reboot_fastboot_command() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));
        let command = transport
            .build_adb_reboot_fastboot_command("ABC123")
            .expect("command should build");
        assert_eq!(
            command.args,
            vec![
                "-s".to_string(),
                "ABC123".to_string(),
                "reboot".to_string(),
                "fastboot".to_string()
            ]
        );
    }

    #[test]
    fn device_transport_builds_targeted_fastboot_reboot_command() {
        let transport = DeviceTransport::new(PlatformTools::new("adb.exe", "fastboot.exe"));
        let command = transport
            .build_fastboot_reboot_target_command("ABC123", Some("bootloader"))
            .expect("targeted reboot command should build");

        assert_eq!(
            command.args,
            vec![
                "-s".to_string(),
                "ABC123".to_string(),
                "reboot".to_string(),
                "bootloader".to_string()
            ]
        );
    }
}
