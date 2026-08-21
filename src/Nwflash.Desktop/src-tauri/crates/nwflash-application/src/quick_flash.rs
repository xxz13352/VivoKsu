//! Minimal quick flash orchestration for partition flash/erase plans.

use std::path::Path;

use crate::command_spec::CommandSpec;
use nwflash_domain::{
    DomainError, FlashImageInfo, PartitionExecutionPlan, PartitionOperationKind,
    PartitionTransportKind,
};
use nwflash_windows::device_transport::DeviceTransport;
use nwflash_windows::platform_tools::PlatformTools;

#[derive(Debug, Clone)]
pub struct QuickFlashService {
    transport: DeviceTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickFlashTaskCommands {
    pub partition_name: String,
    pub commands: Vec<CommandSpec>,
    pub staging_path: Option<String>,
    pub cleanup_command: Option<CommandSpec>,
}

impl QuickFlashService {
    pub const DEFAULT_ADB_EXECUTABLE: &'static str = "adb.exe";
    pub const DEFAULT_FASTBOOT_EXECUTABLE: &'static str = "fastboot.exe";

    pub fn new(transport: DeviceTransport) -> Self {
        Self { transport }
    }

    /// Uses the `adb.exe`/`fastboot.exe` shipped under `resources/platform-tools`
    /// so flashing works on machines without Android platform-tools on `PATH`.
    pub fn with_default_tools() -> Self {
        Self::new(DeviceTransport::new(PlatformTools::bundled()))
    }

    pub fn with_platform_tools(
        adb_executable: impl Into<String>,
        fastboot_executable: impl Into<String>,
    ) -> Self {
        let tools = PlatformTools::new(adb_executable, fastboot_executable);
        Self::new(DeviceTransport::new(tools))
    }

    pub fn inspect_image(&self, image_path: &Path) -> Result<FlashImageInfo, DomainError> {
        let extension = image_path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        if !matches!(extension.as_deref(), Some("img" | "bin")) {
            return Err(DomainError::InvalidInput(
                "快速刷写仅接受 .img 或 .bin 镜像文件。".to_string(),
            ));
        }
        let metadata = std::fs::metadata(image_path)
            .map_err(|_| DomainError::InvalidInput("未找到镜像文件。".to_string()))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(DomainError::InvalidInput(
                "镜像文件为空，无法刷写。".to_string(),
            ));
        }
        let path = image_path.to_str().ok_or_else(|| {
            DomainError::InvalidInput("镜像文件路径包含不支持的字符。".to_string())
        })?;
        Ok(FlashImageInfo {
            path: path.to_string(),
            size_bytes: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        })
    }

    pub fn build_commands(
        &self,
        plan: &PartitionExecutionPlan,
    ) -> Result<Vec<CommandSpec>, DomainError> {
        Ok(self
            .build_task_commands(plan)?
            .into_iter()
            .flat_map(|task| task.commands)
            .collect())
    }

    pub fn retarget_execution_plan(
        &self,
        plan: &PartitionExecutionPlan,
        current_serial: &str,
    ) -> Result<PartitionExecutionPlan, DomainError> {
        if current_serial.trim().is_empty() {
            return Err(DomainError::InvalidInput(
                "设备序列号不能为空。".to_string(),
            ));
        }

        let mut execution_plan = plan.clone();
        execution_plan.serial = current_serial.to_string();
        Ok(execution_plan)
    }

    pub fn build_task_commands(
        &self,
        plan: &PartitionExecutionPlan,
    ) -> Result<Vec<QuickFlashTaskCommands>, DomainError> {
        if plan.serial.trim().is_empty() {
            return Err(DomainError::InvalidInput(
                "设备序列号不能为空。".to_string(),
            ));
        }

        if plan.tasks.is_empty() {
            return Err(DomainError::InvalidOperation(
                "请至少选择一个可执行分区任务。".to_string(),
            ));
        }

        plan.tasks
            .iter()
            .enumerate()
            .map(|(index, task)| {
                let (commands, staging_path, cleanup_command) = match plan.operation {
                    PartitionOperationKind::Write => match plan.transport {
                        PartitionTransportKind::Fastboot => (
                            vec![self.build_fastboot_flash(task, &plan.serial)?],
                            None,
                            None,
                        ),
                        PartitionTransportKind::AdbRoot => {
                            let image_path = task.image_path.as_ref().ok_or_else(|| {
                                DomainError::InvalidOperation("分区镜像路径不能为空。".to_string())
                            })?;
                            let staging_path = adb_root_staging_path(index);
                            let upload = self.transport.build_adb_push_to_staging_command(
                                &plan.serial,
                                image_path,
                                &staging_path,
                            )?;
                            let write = self
                                .transport
                                .build_adb_root_copy_staged_file_to_device_command(
                                    &plan.serial,
                                    &staging_path,
                                    &task.device_path,
                                )?;
                            let cleanup = self
                                .transport
                                .build_adb_remove_staging_command(&plan.serial, &staging_path)?;
                            (
                                vec![upload.into(), write.into()],
                                Some(staging_path),
                                Some(cleanup.into()),
                            )
                        }
                        PartitionTransportKind::Automatic => {
                            return Err(DomainError::InvalidOperation(
                                "执行计划必须使用已解析的设备通道。".to_string(),
                            ));
                        }
                    },
                    PartitionOperationKind::Erase => match plan.transport {
                        PartitionTransportKind::Fastboot => (
                            vec![self
                                .transport
                                .build_fastboot_erase_command(&plan.serial, &task.partition_name)?
                                .into()],
                            None,
                            None,
                        ),
                        PartitionTransportKind::AdbRoot => (
                            vec![self
                                .transport
                                .build_adb_root_erase_command(&plan.serial, &task.device_path)?
                                .into()],
                            None,
                            None,
                        ),
                        PartitionTransportKind::Automatic => {
                            return Err(DomainError::InvalidOperation(
                                "执行计划必须使用已解析的设备通道。".to_string(),
                            ));
                        }
                    },
                    PartitionOperationKind::Backup => {
                        return Err(DomainError::InvalidOperation(
                            "Quick Flash 当前不支持备份操作。".to_string(),
                        ));
                    }
                };

                Ok(QuickFlashTaskCommands {
                    partition_name: task.partition_name.clone(),
                    commands,
                    staging_path,
                    cleanup_command,
                })
            })
            .collect()
    }

    fn build_fastboot_flash(
        &self,
        task: &nwflash_domain::PartitionTask,
        serial: &str,
    ) -> Result<CommandSpec, DomainError> {
        let image_path = task
            .image_path
            .as_ref()
            .ok_or_else(|| DomainError::InvalidOperation("分区镜像路径不能为空。".to_string()))?;

        self.transport
            .build_fastboot_flash_command(serial, &task.partition_name, image_path)
            .map(CommandSpec::from)
    }
}

fn adb_root_staging_path(task_index: usize) -> String {
    format!(
        "/data/local/tmp/nwflash-stage-{}-{task_index}.img",
        std::process::id()
    )
}
