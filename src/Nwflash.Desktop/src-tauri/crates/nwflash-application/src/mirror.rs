//! Safe scrcpy mirror command construction and auto-mirror state.

use std::path::{Path, PathBuf};

use nwflash_domain::DomainError;

use crate::CommandSpec;

#[derive(Debug, Clone)]
pub struct MirrorService {
    scrcpy_executable: PathBuf,
    adb_executable: PathBuf,
    auto_mirror_enabled: bool,
    deliberate_stop: bool,
}

impl MirrorService {
    pub fn new(scrcpy_executable: impl Into<PathBuf>, adb_executable: impl Into<PathBuf>) -> Self {
        Self {
            scrcpy_executable: scrcpy_executable.into(),
            adb_executable: adb_executable.into(),
            auto_mirror_enabled: false,
            deliberate_stop: false,
        }
    }

    pub fn set_auto_mirror_enabled(&mut self, enabled: bool) {
        self.auto_mirror_enabled = enabled;
        if enabled {
            self.deliberate_stop = false;
        }
    }

    pub fn stop(&mut self) {
        self.deliberate_stop = true;
    }

    pub fn reconcile_command(
        &self,
        serial: &str,
        is_adb_connected: bool,
    ) -> Result<Option<CommandSpec>, DomainError> {
        if !self.auto_mirror_enabled || self.deliberate_stop || !is_adb_connected {
            return Ok(None);
        }
        self.build_start_command(serial, true).map(Some)
    }

    pub fn build_start_command(
        &self,
        serial: &str,
        is_adb_connected: bool,
    ) -> Result<CommandSpec, DomainError> {
        if !is_adb_connected || serial.trim().is_empty() {
            return Err(DomainError::InvalidInput(
                "ADB 设备未就绪，无法启动投屏。".to_string(),
            ));
        }
        if !self.scrcpy_executable.is_file() {
            return Err(DomainError::InvalidInput(
                "未检测到 scrcpy.exe，无法启动投屏。".to_string(),
            ));
        }
        if !self.adb_executable.is_file() {
            return Err(DomainError::InvalidInput(
                "未检测到 platform-tools adb.exe，无法启动投屏。".to_string(),
            ));
        }

        Ok(CommandSpec {
            program: path_to_string(&self.scrcpy_executable)?,
            args: vec![
                "--serial".to_string(),
                serial.to_string(),
                "--stay-awake".to_string(),
            ],
            working_directory: None,
            environment: vec![("ADB".to_string(), path_to_string(&self.adb_executable)?)],
        })
    }
}

fn path_to_string(path: &Path) -> Result<String, DomainError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| DomainError::InvalidInput("工具路径包含不支持的字符。".to_string()))
}
