//! Safe ADB file-manager command construction.

use std::path::Path;

use nwflash_domain::{DeviceFileEntry, DomainError};
use nwflash_windows::platform_tools::PlatformTools;

use crate::CommandSpec;

#[derive(Debug, Clone)]
pub struct FileManagerService {
    tools: PlatformTools,
}

impl FileManagerService {
    /// Uses the `adb.exe`/`fastboot.exe` shipped under `resources/platform-tools`
    /// so file operations work on machines without platform-tools on `PATH`.
    pub fn bundled() -> Self {
        Self {
            tools: PlatformTools::bundled(),
        }
    }

    pub fn with_platform_tools(
        adb_executable: impl Into<String>,
        fastboot_executable: impl Into<String>,
    ) -> Self {
        Self {
            tools: PlatformTools::new(adb_executable, fastboot_executable),
        }
    }

    pub fn build_pull_command(
        &self,
        serial: &str,
        remote_file: &str,
        local_destination: &Path,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_remote_path(remote_file)?;
        validate_download_destination(local_destination)?;
        self.tools
            .adb_command(
                serial,
                &[
                    "pull".to_string(),
                    remote_file.to_string(),
                    local_destination.to_string_lossy().into_owned(),
                ],
            )
            .map(CommandSpec::from)
    }

    pub fn build_push_command(
        &self,
        serial: &str,
        local_source: &Path,
        remote_directory: &str,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_local_source(local_source)?;
        validate_remote_path(remote_directory)?;
        let name = local_source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| DomainError::InvalidInput("本地文件名无效。".to_string()))?;
        let remote_destination = join_remote_path(remote_directory, name);
        self.tools
            .adb_command(
                serial,
                &[
                    "push".to_string(),
                    local_source.to_string_lossy().into_owned(),
                    remote_destination,
                ],
            )
            .map(CommandSpec::from)
    }

    pub fn build_list_command(
        &self,
        serial: &str,
        remote_directory: &str,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_remote_path(remote_directory)?;
        let directory = directory_for_listing(remote_directory);
        self.tools
            .adb_command(
                serial,
                &[
                    "shell".to_string(),
                    format!("ls -laL -- {}", quote_remote_path(&directory)),
                ],
            )
            .map(CommandSpec::from)
    }

    pub fn build_delete_command(
        &self,
        serial: &str,
        remote_path: &str,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_remote_path(remote_path)?;
        if remote_path == "/" {
            return Err(DomainError::InvalidInput(
                "不允许删除设备根目录。".to_string(),
            ));
        }
        self.tools
            .adb_command(
                serial,
                &[
                    "shell".to_string(),
                    format!("rm -rf -- {}", quote_remote_path(remote_path)),
                ],
            )
            .map(CommandSpec::from)
    }

    pub fn build_install_apk_command(
        &self,
        serial: &str,
        apk_path: &Path,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_local_source(apk_path)?;
        if !apk_path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("apk"))
        {
            return Err(DomainError::InvalidInput("只能安装 APK 文件。".to_string()));
        }
        self.tools
            .adb_command(
                serial,
                &[
                    "install".to_string(),
                    "-r".to_string(),
                    apk_path.to_string_lossy().into_owned(),
                ],
            )
            .map(CommandSpec::from)
    }
}

fn validate_serial(serial: &str) -> Result<(), DomainError> {
    if serial.trim().is_empty() {
        return Err(DomainError::InvalidInput("设备串口不能为空。".to_string()));
    }
    Ok(())
}

fn validate_remote_path(path: &str) -> Result<(), DomainError> {
    if !path.starts_with('/')
        || path.contains('\0')
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(DomainError::InvalidInput(
            "设备路径必须是非穿越的绝对路径。".to_string(),
        ));
    }
    Ok(())
}

fn validate_download_destination(path: &Path) -> Result<(), DomainError> {
    let parent = path
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| DomainError::InvalidInput("下载目标目录不存在。".to_string()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| DomainError::InvalidInput("下载目标路径无效。".to_string()))?;
    if !parent.is_absolute() {
        return Err(DomainError::InvalidInput("下载目标路径无效。".to_string()));
    }
    if !is_safe_windows_file_name(file_name) {
        return Err(DomainError::InvalidInput(
            "设备文件名无法安全保存到 Windows。".to_string(),
        ));
    }
    Ok(())
}

/// Rejects file names Windows cannot represent faithfully: reserved device
/// names (CON/PRN/AUX/NUL, COM1-9/LPT1-9), trailing dots/spaces (which the
/// Win32 APIs silently strip), separators, control characters, and the other
/// invalid filename characters. Mirrors the WPF `ValidateSafeFileName`.
fn is_safe_windows_file_name(name: &str) -> bool {
    if name.trim().is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return false;
    }
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    if name
        .chars()
        .any(|ch| (ch as u32) < 32 || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return false;
    }

    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return false;
    }
    if stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit()
        && (b'1'..=b'9').contains(&stem.as_bytes()[3])
    {
        return false;
    }

    true
}

fn validate_local_source(path: &Path) -> Result<(), DomainError> {
    if !path.is_file() {
        return Err(DomainError::InvalidInput(
            "本地上传文件不存在。".to_string(),
        ));
    }
    Ok(())
}

fn join_remote_path(directory: &str, name: &str) -> String {
    if directory == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", directory.trim_end_matches('/'), name)
    }
}

fn directory_for_listing(directory: &str) -> String {
    if directory == "/" {
        "/".to_string()
    } else {
        format!("{}/", directory.trim_end_matches('/'))
    }
}

fn quote_remote_path(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

pub fn parse_remote_listing(directory: &str, output: &str) -> Vec<DeviceFileEntry> {
    let mut entries = output
        .lines()
        .filter_map(|line| parse_listing_entry(directory, line))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    entries
}

fn parse_listing_entry(directory: &str, line: &str) -> Option<DeviceFileEntry> {
    let mut remaining = line.trim_start();
    let mode = take_listing_field(&mut remaining)?;
    if !matches!(
        mode.chars().next(),
        Some('b' | 'c' | 'd' | 'l' | 'p' | 's' | '-')
    ) {
        return None;
    }
    take_listing_field(&mut remaining)?;
    take_listing_field(&mut remaining)?;
    take_listing_field(&mut remaining)?;
    let size_bytes = take_listing_field(&mut remaining)?.parse::<i64>().ok()?;
    take_listing_field(&mut remaining)?;
    take_listing_field(&mut remaining)?;
    let name = remaining
        .trim()
        .split_once(" -> ")
        .map_or_else(|| remaining.trim(), |(name, _)| name);
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }

    Some(DeviceFileEntry {
        name: name.to_string(),
        full_path: join_remote_path(directory, name),
        is_directory: mode.starts_with('d'),
        size_bytes,
    })
}

fn take_listing_field<'a>(remaining: &mut &'a str) -> Option<&'a str> {
    *remaining = remaining.trim_start();
    let end = remaining
        .find(char::is_whitespace)
        .unwrap_or(remaining.len());
    if end == 0 {
        return None;
    }
    let field = &remaining[..end];
    *remaining = &remaining[end..];
    Some(field)
}

#[cfg(test)]
mod tests {
    use super::is_safe_windows_file_name;

    #[test]
    fn rejects_windows_reserved_device_names_and_com_lpt_ports() {
        for name in [
            "con", "CON.txt", "prn", "aux", "nul", "com1", "com9", "lpt1", "LPT9",
        ] {
            assert!(
                !is_safe_windows_file_name(name),
                "{name} should be rejected as a reserved Windows name"
            );
        }
    }

    #[test]
    fn accepts_ordinary_file_names_but_rejects_trailing_dots_spaces_and_separators() {
        assert!(is_safe_windows_file_name("notes.txt"));
        assert!(is_safe_windows_file_name("boot.img"));
        assert!(!is_safe_windows_file_name("notes."));
        assert!(!is_safe_windows_file_name("notes "));
        assert!(!is_safe_windows_file_name("a/b"));
        assert!(!is_safe_windows_file_name("a\\b"));
        assert!(!is_safe_windows_file_name("a<b>c"));
        assert!(!is_safe_windows_file_name("."));
        assert!(!is_safe_windows_file_name(".."));
        assert!(is_safe_windows_file_name("com0")); // COM0 is not reserved (only COM1-9)
    }
}
