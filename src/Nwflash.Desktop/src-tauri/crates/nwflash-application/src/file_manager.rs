//! Safe ADB file-manager command construction.

use std::path::Path;

use nwflash_domain::{DeviceFileEntry, DomainError};
use nwflash_windows::{file_ops::ensure_safe_directory, platform_tools::PlatformTools};

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
        self.build_pull_command_to_path(serial, remote_file, local_destination)
    }

    /// Builds an ADB pull command for an explicitly selected local path.
    /// Transactional callers use this for a sibling temporary file while the
    /// legacy public method above keeps the same final-path behavior.
    pub fn build_pull_command_to_path(
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
        let remote_destination = self.push_destination(serial, local_source, remote_directory)?;
        self.build_push_command_to_path(serial, local_source, &remote_destination)
    }

    /// Returns the final remote path used by a normal file-manager upload.
    pub fn push_destination(
        &self,
        serial: &str,
        local_source: &Path,
        remote_directory: &str,
    ) -> Result<String, DomainError> {
        validate_serial(serial)?;
        validate_local_source(local_source)?;
        validate_remote_path(remote_directory)?;
        let name = local_source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| DomainError::InvalidInput("本地文件名无效。".to_string()))?;
        Ok(join_remote_path(remote_directory, name))
    }

    /// Builds an ADB push command for an explicitly selected remote path.
    /// The destination is still validated as an absolute, non-traversing
    /// device path; callers use it for a transaction-owned temporary name.
    pub fn build_push_command_to_path(
        &self,
        serial: &str,
        local_source: &Path,
        remote_destination: &str,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_local_source(local_source)?;
        validate_remote_path(remote_destination)?;
        self.tools
            .adb_command(
                serial,
                &[
                    "push".to_string(),
                    local_source.to_string_lossy().into_owned(),
                    remote_destination.to_string(),
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

    /// Builds a shell command that promotes a temporary remote file without
    /// overwriting an existing destination. A plain `mv` is deliberately not
    /// used: unsupported/no-replace behavior must fail closed.
    pub fn build_remote_promote_command(
        &self,
        serial: &str,
        temporary_path: &str,
        destination_path: &str,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_remote_path(temporary_path)?;
        validate_remote_path(destination_path)?;
        if temporary_path == "/" || destination_path == "/" || temporary_path == destination_path {
            return Err(DomainError::InvalidInput(
                "远端暂存路径或目标路径无效。".to_string(),
            ));
        }

        let temporary = quote_remote_path(temporary_path);
        let destination = quote_remote_path(destination_path);
        let script = format!(
            "if [ ! -f {temporary} ] || [ -L {temporary} ]; then exit 72; fi; \
             if [ -e {destination} ] || [ -L {destination} ]; then exit 73; fi; \
             mv -n -- {temporary} {destination}; status=$?; \
             if [ \"$status\" -ne 0 ] || [ -e {temporary} ] || [ -L {temporary} ] || [ ! -f {destination} ] || [ -L {destination} ]; then exit 74; fi; \
             exit 0"
        );
        // 与 C# `ShellAsync` 一致：整段脚本作为一个参数交给设备端 shell。
        // 不能写成 `shell -T sh -c <脚本>`：adb 会把 argv 用空格拼接后发给
        // 设备，脚本里的 `;`/空格会被设备端 shell 当成语法拆散（实测
        // `sh -c if [ -d /data/local/tmp ]; then ...` 直接报
        // `syntax error: unexpected 'then'`，退出码 1）。
        self.tools
            .adb_command(serial, &["shell".to_string(), script])
            .map(CommandSpec::from)
    }

    /// Explicitly named alias for callers that want the no-replace contract
    /// visible at the call site.  Keep the shorter historical name above for
    /// compatibility with the first transaction implementation.
    pub fn build_remote_promote_no_replace_command(
        &self,
        serial: &str,
        temporary_path: &str,
        destination_path: &str,
    ) -> Result<CommandSpec, DomainError> {
        self.build_remote_promote_command(serial, temporary_path, destination_path)
    }

    /// Builds an idempotent cleanup command for one transaction-owned remote
    /// temporary file. The path is never a wildcard or a directory.  The
    /// command also verifies that both a normal entry and a broken symlink are
    /// absent after `rm`, so an exit code of zero alone is not treated as
    /// proof that cleanup completed.
    pub fn build_remote_remove_file_command(
        &self,
        serial: &str,
        temporary_path: &str,
    ) -> Result<CommandSpec, DomainError> {
        validate_serial(serial)?;
        validate_remote_path(temporary_path)?;
        if temporary_path == "/" {
            return Err(DomainError::InvalidInput("远端暂存路径无效。".to_string()));
        }
        let temporary = quote_remote_path(temporary_path);
        let script = format!(
            "rm -f -- {temporary}; status=$?; \
             if [ \"$status\" -ne 0 ] || [ -e {temporary} ] || [ -L {temporary} ]; then exit 74; fi; \
             exit 0"
        );
        // 同上：整段脚本交给设备端 shell，不要用 `sh -c`（adb 参数拼接会拆散它）。
        self.tools
            .adb_command(serial, &["shell".to_string(), script])
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
    ensure_safe_directory(parent)
        .map_err(|_| DomainError::InvalidInput("下载目标目录不可安全使用。".to_string()))?;
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
    nwflash_windows::file_ops::ensure_safe_regular_file(path)
        .map_err(|_| DomainError::InvalidInput("本地上传文件不存在。".to_string()))?;
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
    // 与 C# LsLine 正则一致：mode 必须是完整的 10 字符权限位
    // （类型 + rwx 三组），畸形行（shell 报错、多空格输出）整行丢弃，
    // 防止解析出伪条目。
    if mode.len() != 10 {
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
