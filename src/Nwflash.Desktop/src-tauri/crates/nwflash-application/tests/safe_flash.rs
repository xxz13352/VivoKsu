use std::{
    collections::{HashSet, VecDeque},
    fs::{self, File},
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::{
    SafeFlashBuildOptions, SafeFlashExecutionRequest, SafeFlashExecutionService,
    SafeFlashPartitionSource, SafeFlashPreparationPhase, SafeFlashPreparedSource, SafeFlashService,
    SafeFlashSource,
};
use nwflash_domain::{DomainError, SafeFlashSlotMode};
use nwflash_windows::process::{CancellableProcessExecutor, ProcessCommand, ProcessOutput};
use tokio_util::sync::CancellationToken;
use wiremock::{
    matchers::{header, method},
    Mock, MockServer, ResponseTemplate,
};
use zip4::{write::SimpleFileOptions, ZipWriter};

#[derive(Clone)]
struct RecordedExecutor {
    commands: Arc<Mutex<Vec<ProcessCommand>>>,
    outputs: Arc<Mutex<VecDeque<Result<ProcessOutput, DomainError>>>>,
}

impl RecordedExecutor {
    fn new(outputs: impl IntoIterator<Item = Result<ProcessOutput, DomainError>>) -> Self {
        Self {
            commands: Arc::new(Mutex::new(Vec::new())),
            outputs: Arc::new(Mutex::new(outputs.into_iter().collect())),
        }
    }

    fn commands(&self) -> Vec<ProcessCommand> {
        self.commands
            .lock()
            .expect("recorded commands lock should not be poisoned")
            .clone()
    }
}

impl CancellableProcessExecutor for RecordedExecutor {
    fn run(
        &self,
        spec: ProcessCommand,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<ProcessOutput, DomainError> {
        if should_cancel() {
            return Err(DomainError::UserCancelled("运行被用户取消".to_string()));
        }
        self.commands
            .lock()
            .expect("recorded commands lock should not be poisoned")
            .push(spec);
        self.outputs
            .lock()
            .expect("recorded outputs lock should not be poisoned")
            .pop_front()
            .expect("test must provide one output for every command")
    }
}

fn successful_output(stdout: &str) -> Result<ProcessOutput, DomainError> {
    Ok(ProcessOutput {
        exit_code: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

fn request_for_other_slot<'a>(
    source: &'a SafeFlashPreparedSource,
    options: &'a SafeFlashBuildOptions,
    transition_to_fastbootd: bool,
) -> SafeFlashExecutionRequest<'a> {
    SafeFlashExecutionRequest {
        source,
        options,
        serial: options.serial.as_str(),
        transition_to_fastbootd,
    }
}

fn make_service() -> SafeFlashService {
    SafeFlashService::new()
}

fn common_partitions() -> Vec<SafeFlashPartitionSource> {
    vec![
        SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\tmp\\boot.img".to_string(),
            has_slot: true,
        },
        SafeFlashPartitionSource {
            partition_name: "init_boot".to_string(),
            image_path: "C:\\tmp\\init_boot.img".to_string(),
            has_slot: true,
        },
        SafeFlashPartitionSource {
            partition_name: "preloader".to_string(),
            image_path: "C:\\tmp\\preloader.img".to_string(),
            has_slot: true,
        },
        SafeFlashPartitionSource {
            partition_name: "vendor_boot".to_string(),
            image_path: "C:\\tmp\\vendor_boot.img".to_string(),
            has_slot: true,
        },
        SafeFlashPartitionSource {
            partition_name: "userdata".to_string(),
            image_path: "C:\\tmp\\userdata.img".to_string(),
            has_slot: true,
        },
    ]
}

#[test]
fn safe_flash_build_plan_filters_safe_and_keep_root_flags() {
    let service = make_service();
    let partitions = common_partitions();
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: true,
        is_keep_root: true,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: Some("a".to_string()),
    };

    let plan = service
        .build_plan(&partitions, options)
        .expect("safe flash plan should build");

    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.tasks[0].partition_name, "userdata");
}

#[test]
fn execution_uses_the_sole_fastbootd_device_after_transition_and_skips_missing_partitions() {
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("ADB-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) current-slot: a\n"),
        successful_output("(bootloader) has-slot:boot: yes\n"),
        successful_output("(bootloader) partition-type:boot: raw\n"),
        successful_output("(bootloader) has-slot:vendor_boot: yes\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "unknown partition".to_string(),
        }),
        successful_output("(bootloader) partition-type:misc: raw\n"),
        successful_output(""),
        successful_output(""),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()));
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            SafeFlashPartitionSource {
                partition_name: "boot".to_string(),
                image_path: "C:\\staging\\boot.img".to_string(),
                has_slot: true,
            },
            SafeFlashPartitionSource {
                partition_name: "vendor_boot".to_string(),
                image_path: "C:\\staging\\vendor_boot.img".to_string(),
                has_slot: true,
            },
        ],
        wipe_data_image_path: Some("C:\\staging\\wipe-data.img".to_string()),
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "ADB-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: true,
        wipe_data_image_path: Some("C:\\staging\\wipe-data.img".to_string()),
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    let result = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: true,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect("recorded fastboot workflow should complete");

    assert_eq!(result.flashed_partition_count, 2);
    assert_eq!(result.skipped_partition_count, 1);
    let commands = executor.commands();
    assert_eq!(commands[0].args, ["-s", "ADB-001", "reboot", "fastboot"]);
    assert_eq!(commands[1].args, ["devices"]);
    let fastboot_commands = commands
        .iter()
        .filter(|command| {
            Path::new(&command.program)
                .file_name()
                .and_then(|name| name.to_str())
                == Some("fastboot.exe")
                && command
                    .args
                    .first()
                    .is_some_and(|argument| argument == "-s")
        })
        .collect::<Vec<_>>();
    assert!(fastboot_commands
        .iter()
        .all(|command| command.args[1] == "ADB-001"));
    assert!(fastboot_commands
        .iter()
        .all(|command| Path::new(&command.program).is_absolute()));
    assert_eq!(
        fastboot_commands
            .iter()
            .filter_map(|command| command.args.get(2))
            .filter(|argument| argument.as_str() == "flash")
            .map(|_| 1usize)
            .sum::<usize>(),
        2
    );
    assert_eq!(commands[commands.len() - 2].args[2], "flash");
    assert_eq!(commands[commands.len() - 2].args[3], "misc");
    assert_eq!(
        commands.last().expect("reboot command expected").args[2],
        "reboot"
    );
}

#[test]
fn execution_rejects_bootloader_fastboot_before_any_flash() {
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: no\n"),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    let error = service
        .execute(request_for_other_slot(&source, &options, false), || false, |_| {}, |_| {})
        .expect_err("bootloader fastboot must not be accepted as fastbootd");

    assert!(error.to_string().contains("fastbootd"));
    assert!(!executor
        .commands()
        .iter()
        .any(|command| command.args.get(2) == Some(&"flash".to_string())));
}

#[test]
fn execution_rejects_multiple_fastboot_devices_before_any_flash() {
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\nFASTBOOT-002\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) partition-type:boot: raw\n"),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: false,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let error = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: "FASTBOOT-001",
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect_err("multiple fastboot devices must be rejected before flashing");

    assert!(error.to_string().contains("多个"));
    assert_eq!(executor.commands().len(), 1);
}

#[test]
fn execution_rejects_unreadable_current_slot_before_partition_probing_or_flash() {
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: current-slot unavailable)".to_string(),
        }),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    let error = service
        .execute(request_for_other_slot(&source, &options, true), || false, |_| {}, |_| {})
        .expect_err("an unreadable current slot must stop safe flash");

    assert!(error.to_string().contains("current-slot"));
    assert!(!executor.commands().iter().any(|command| {
        command.args.get(3).is_some_and(|argument| argument.starts_with("partition-type:"))
            || command.args.get(2) == Some(&"flash".to_string())
    }));
}

#[test]
fn execution_rejects_unreadable_has_slot_before_partition_probing_or_flash() {
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) current-slot: a\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: has-slot unavailable)".to_string(),
        }),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    let error = service
        .execute(request_for_other_slot(&source, &options, true), || false, |_| {}, |_| {})
        .expect_err("an unreadable has-slot fact must stop safe flash");

    assert!(error.to_string().contains("has-slot:boot"));
    assert!(!executor.commands().iter().any(|command| {
        command.args.get(3).is_some_and(|argument| argument.starts_with("partition-type:"))
            || command.args.get(2) == Some(&"flash".to_string())
    }));
}

#[test]
fn execution_rejects_unrecognized_has_slot_before_partition_probing_or_flash() {
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) current-slot: a\n"),
        successful_output("(bootloader) has-slot:boot: unknown\n"),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    let error = service
        .execute(request_for_other_slot(&source, &options, true), || false, |_| {}, |_| {})
        .expect_err("an unrecognized has-slot value must stop safe flash");

    assert!(error.to_string().contains("未读取到有效 has-slot:boot 值"));
    assert!(!executor.commands().iter().any(|command| {
        command.args.get(3).is_some_and(|argument| argument.starts_with("partition-type:"))
            || command.args.get(2) == Some(&"flash".to_string())
    }));
}

#[test]
fn execution_uses_the_current_target_when_it_differs_from_preflight_target() {
    let executor = RecordedExecutor::new([
        successful_output("CURRENT-FASTBOOT\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) partition-type:boot: raw\n"),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()));
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: false,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "PRECHECK-STALE".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let result = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: "CURRENT-FASTBOOT",
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect("a prepared flash must target the sole current fastboot device");

    assert_eq!(result.flashed_partition_count, 1);
    let commands = executor.commands();
    assert_eq!(commands[0].args, ["devices"]);
    assert!(commands.iter().skip(1).all(|command| {
        command.args.first().map(String::as_str) == Some("-s")
            && command.args.get(1).map(String::as_str) == Some("CURRENT-FASTBOOT")
    }));
}

#[test]
fn execution_uses_the_sole_fastbootd_device_when_its_serial_differs_from_adb() {
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("OTHER-DEVICE\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) partition-type:init_boot: raw\n"),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "init_boot".to_string(),
            image_path: "C:\\staging\\init_boot.img".to_string(),
            has_slot: false,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "ADB-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let result = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: true,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect("the sole current fastbootd device must be used even when its serial changed");

    assert_eq!(result.flashed_partition_count, 1);
    let commands = executor.commands();
    assert_eq!(commands[0].args, ["-s", "ADB-001", "reboot", "fastboot"]);
    assert_eq!(commands[1].args, ["devices"]);
    assert!(commands.iter().skip(2).all(|command| {
        command.args.first().map(String::as_str) == Some("-s")
            && command.args.get(1).map(String::as_str) == Some("OTHER-DEVICE")
    }));
}

#[test]
fn execution_rejects_network_adb_before_attempting_fastbootd_transition() {
    let executor = RecordedExecutor::new([Err(DomainError::ExternalTool(
        "the network ADB device must be rejected before this command runs".to_string(),
    ))]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()));
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: false,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "192.168.1.2:5555".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let error = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: true,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect_err("network ADB must be rejected before a fastbootd transition");

    assert!(error.to_string().contains("USB"));
    assert!(executor.commands().is_empty());
}

#[test]
fn execution_reports_fastbootd_timeout_without_attempting_partition_preflight() {
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("<waiting for any device>\n"),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "ADB-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let error = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: true,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect_err("a device that never reaches fastbootd must stop before preflight");

    assert!(error.to_string().contains("fastbootd"));
    assert_eq!(executor.commands().len(), 2);
}

#[test]
fn execution_stops_after_the_first_flash_failure_without_rebooting() {
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) partition-type:boot: raw\n"),
        successful_output("(bootloader) partition-type:vendor_boot: raw\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED device=FASTBOOT-SECRET token=secret https://rom.invalid/private.zip"
                .to_string(),
        }),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()));
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            SafeFlashPartitionSource {
                partition_name: "boot".to_string(),
                image_path: "C:\\staging\\boot.img".to_string(),
                has_slot: true,
            },
            SafeFlashPartitionSource {
                partition_name: "vendor_boot".to_string(),
                image_path: "C:\\staging\\vendor_boot.img".to_string(),
                has_slot: true,
            },
        ],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let error = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect_err("the first failed flash must stop the workflow");

    let message = error.to_string();
    assert!(message.contains("fastboot 命令执行失败"));
    assert!(!message.contains("FASTBOOT-SECRET"));
    assert!(!message.contains("secret"));
    assert!(!message.contains("rom.invalid"));
    let commands = executor.commands();
    assert_eq!(commands.len(), 5);
    assert_eq!(
        commands.last().expect("failed flash expected").args[2],
        "flash"
    );
    assert!(!commands.iter().any(|command| command
        .args
        .get(2)
        .is_some_and(|argument| argument == "reboot")));
}

#[test]
fn execution_reads_fastboot_slot_variables_from_stderr() {
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: "(bootloader) current-slot: a\n".to_string(),
        }),
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: String::new(),
            stderr: "(bootloader) has-slot:boot: yes\n".to_string(),
        }),
        successful_output("(bootloader) partition-type:boot_b: raw\n"),
        successful_output(""),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()));
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
        )
        .expect("stderr getvar output should preserve other-slot flashing");

    let commands = executor.commands();
    assert!(commands.iter().any(|command| {
        command
            .args
            .get(2)
            .is_some_and(|argument| argument == "flash")
            && command
                .args
                .get(3)
                .is_some_and(|argument| argument == "boot_b")
    }));
    assert!(commands.iter().any(|command| {
        command
            .args
            .get(2)
            .is_some_and(|argument| argument == "set_active")
            && command.args.get(3).is_some_and(|argument| argument == "b")
    }));
}

#[test]
fn execution_cancellation_before_the_first_flash_does_not_reboot() {
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) partition-type:boot: raw\n"),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()));
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
        }],
        wipe_data_image_path: None,
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let mut cancellation_checks = 0usize;

    let error = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || {
                cancellation_checks += 1;
                cancellation_checks >= 9
            },
            |_| {},
            |_| {},
        )
        .expect_err("cancellation before the first flash must stop the workflow");

    assert!(matches!(error, DomainError::UserCancelled(_)));
    let commands = executor.commands();
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[2].args[2], "getvar");
    assert!(!commands.iter().any(|command| command
        .args
        .get(2)
        .is_some_and(|argument| argument == "reboot")));
}

#[test]
fn disabling_safe_flash_keeps_preloader_and_lk_in_the_flash_plan() {
    let service = make_service();
    let partitions = vec![
        SafeFlashPartitionSource {
            partition_name: "preloader_raw".to_string(),
            image_path: "C:\\tmp\\preloader.img".to_string(),
            has_slot: false,
        },
        SafeFlashPartitionSource {
            partition_name: "lk".to_string(),
            image_path: "C:\\tmp\\lk.img".to_string(),
            has_slot: false,
        },
        SafeFlashPartitionSource {
            partition_name: "vbmeta".to_string(),
            image_path: "C:\\tmp\\vbmeta.img".to_string(),
            has_slot: false,
        },
    ];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let plan = service
        .build_plan(&partitions, options)
        .expect("all partitions should remain when the safe filter is disabled");
    assert_eq!(
        plan.tasks
            .iter()
            .map(|task| task.partition_name.as_str())
            .collect::<Vec<_>>(),
        ["preloader_raw", "lk", "vbmeta"]
    );
}

#[test]
fn safe_flash_build_plan_expands_slot_targets() {
    let service = make_service();
    let partitions = vec![SafeFlashPartitionSource {
        partition_name: "boot".to_string(),
        image_path: "C:\\tmp\\boot.img".to_string(),
        has_slot: true,
    }];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::BothSlots,
        current_slot: Some("a".to_string()),
    };

    let plan = service
        .build_plan(&partitions, options)
        .expect("safe flash both-slots plan should build");

    let task_names: Vec<_> = plan
        .tasks
        .iter()
        .map(|task| task.partition_name.as_str())
        .collect();
    assert_eq!(task_names, ["boot_a", "boot_b"]);
}

#[test]
fn safe_flash_build_plan_rejects_missing_wipe_data_path() {
    let service = make_service();
    let partitions = vec![SafeFlashPartitionSource {
        partition_name: "userdata".to_string(),
        image_path: "C:\\tmp\\userdata.img".to_string(),
        has_slot: true,
    }];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: true,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: Some("a".to_string()),
    };

    let err = service
        .build_plan(&partitions, options)
        .expect_err("wipe-data requires image path");
    assert!(err.to_string().contains("清除数据镜像路径不能为空"));
}

#[test]
fn safe_flash_build_plan_appends_wipe_task_last() {
    let service = make_service();
    let partitions = vec![SafeFlashPartitionSource {
        partition_name: "boot".to_string(),
        image_path: "C:\\tmp\\boot.img".to_string(),
        has_slot: true,
    }];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: true,
        wipe_data_image_path: Some("C:\\tmp\\wipe-data.img".to_string()),
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: Some("a".to_string()),
    };

    let plan = service
        .build_plan(&partitions, options)
        .expect("safe flash wipe plan should build");
    assert_eq!(
        plan.tasks
            .last()
            .expect("tasks should not be empty")
            .partition_name,
        "misc"
    );
}

#[test]
fn safe_flash_commands_share_quick_flash_transport() {
    let service = make_service();
    let partitions = vec![SafeFlashPartitionSource {
        partition_name: "boot".to_string(),
        image_path: "C:\\tmp\\boot.img".to_string(),
        has_slot: true,
    }];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        wipe_data_image_path: None,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: Some("a".to_string()),
    };

    let _commands = service
        .build_commands(&partitions, options)
        .expect("commands should build");
}

#[tokio::test]
async fn local_zip_extraction_uses_private_staging_without_writing_beside_the_source() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-local-zip-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).expect("zip should be created"));
    archive
        .start_file("images/boot.img", SimpleFileOptions::default())
        .expect("zip entry should be created");
    std::io::Write::write_all(&mut archive, b"boot").expect("zip image should be written");
    archive.finish().expect("zip should be finalized");

    let prepared = SafeFlashService::new()
        .resolve_source(
            SafeFlashSource::LocalPath {
                path: archive_path.to_string_lossy().into_owned(),
            },
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        )
        .await
        .expect("local zip should prepare a private image staging directory");

    let staging_root = prepared
        .staging_root
        .expect("local zip extraction must own a private staging root");
    assert!(prepared.partitions[0]
        .image_path
        .starts_with(staging_root.to_string_lossy().as_ref()));
    assert!(!root.join("boot.img").exists());

    fs::remove_dir_all(&staging_root).expect("private staging should be removable");
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn local_zip_preparation_reports_monotonic_byte_progress_through_completion() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-progress-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).expect("zip should be created"));
    archive
        .start_file("images/boot.img", SimpleFileOptions::default())
        .expect("boot entry should be created");
    std::io::Write::write_all(&mut archive, &[1u8; 128 * 1024])
        .expect("boot image should be written");
    archive.finish().expect("zip should be finalized");
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_for_sink = progress.clone();

    let prepared = SafeFlashService::new()
        .resolve_source_with_cancellation_and_progress(
            SafeFlashSource::LocalPath {
                path: archive_path.to_string_lossy().into_owned(),
            },
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
            &CancellationToken::new(),
            None,
            Some(Arc::new(move |_, completed, total| {
                progress_for_sink
                    .lock()
                    .expect("progress lock should be available")
                    .push((completed, total));
            })),
        )
        .await
        .expect("local ZIP should prepare successfully");

    let reported = progress.lock().expect("progress lock should be available");
    assert!(reported
        .iter()
        .any(|(completed, total)| *completed > 0 && *completed < *total));
    assert_eq!(reported.last(), Some(&(128 * 1024, 128 * 1024)));
    assert!(reported.windows(2).all(|pair| pair[0].0 <= pair[1].0));

    fs::remove_dir_all(prepared.staging_root.expect("staging should exist"))
        .expect("staging should be removable");
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn cancelled_local_preparation_uses_the_callers_cancellation_token() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-cancelled-local-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).expect("zip should be created"));
    archive
        .start_file("images/boot.img", SimpleFileOptions::default())
        .expect("zip entry should be created");
    std::io::Write::write_all(&mut archive, b"boot").expect("zip image should be written");
    archive.finish().expect("zip should be finalized");
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = SafeFlashService::new()
        .resolve_source_with_cancellation(
            SafeFlashSource::LocalPath {
                path: archive_path.to_string_lossy().into_owned(),
            },
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
            &cancellation,
            None,
        )
        .await
        .expect_err("cancelled preparation must not read or extract the local archive");

    assert!(matches!(error, DomainError::UserCancelled(_)));
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn online_source_prepares_equal_length_content_without_a_catalog_hash_gate() {
    let root = std::env::temp_dir().join(format!(
        "nwflash-safe-flash-online-integrity-mismatch-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be available")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("online.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).expect("zip should be created"));
    archive
        .start_file("boot.img", SimpleFileOptions::default())
        .expect("boot entry should be created");
    std::io::Write::write_all(&mut archive, b"boot").expect("boot image should be written");
    archive.finish().expect("zip should be finalized");
    let archive_bytes = fs::read(&archive_path).expect("fixture archive should be readable");
    let server = MockServer::start().await;
    let length = archive_bytes.len().to_string();
    Mock::given(method("HEAD"))
        .respond_with(ResponseTemplate::new(200).insert_header("content-length", length.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive_bytes.clone()))
        .mount(&server)
        .await;

    let prepared = SafeFlashService::new()
        .resolve_source_with_cancellation(
            SafeFlashSource::Online {
                url: server.uri(),
                pd: "PD2057".to_string(),
                version: "16.2.10.0".to_string(),
                payload_dumper: None,
            },
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("online OTA preparation must not reject equal-length altered content by hash");

    assert_eq!(prepared.partitions.len(), 1);
    assert_eq!(prepared.partitions[0].partition_name, "boot");
    if let Some(staging_root) = prepared.staging_root {
        fs::remove_dir_all(staging_root).expect("Safe Flash staging should be removed");
    }
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[tokio::test]
async fn online_payload_zip_uses_the_controlled_dumper_and_discards_download_staging() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-online-payload-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let safe_flash_parent = std::env::temp_dir().join("nwflash-safe-flash");
    let before_staging = fs::read_dir(&safe_flash_parent)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<HashSet<_>>();
    let archive_path = root.join("online-payload.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).expect("zip should be created"));
    archive
        .start_file(
            "firmware/payload.bin",
            SimpleFileOptions::default().last_modified_time(zip4::DateTime::default()),
        )
        .expect("payload entry should be created");
    std::io::Write::write_all(&mut archive, b"CrAU-online-payload")
        .expect("payload entry should be written");
    archive.finish().expect("zip should be finalized");
    let archive_bytes = fs::read(&archive_path).expect("fixture archive should be readable");
    let server = MockServer::start().await;
    let length = archive_bytes.len().to_string();
    Mock::given(method("HEAD"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", length.as_str())
                .insert_header("accept-ranges", "bytes"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(header(
            "range",
            format!("bytes=0-{}", archive_bytes.len() - 1).as_str(),
        ))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-length", length.as_str())
                .insert_header(
                    "content-range",
                    format!(
                        "bytes 0-{}/{}",
                        archive_bytes.len() - 1,
                        archive_bytes.len()
                    )
                    .as_str(),
                )
                .set_body_bytes(archive_bytes.clone()),
        )
        .mount(&server)
        .await;
    let tool = root.join("payload_dumper.cmd");
    fs::write(
        &tool,
        "@echo off\r\nset output=\r\nset metadata=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"--metadata\" set metadata=1\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\nif defined metadata ( >\"%output%\\metadata.json\" echo {\"partitions\":[{\"partition_name\":\"boot\",\"size_in_bytes\":9}]} ) else >\"%output%\\boot.img\" echo payload\r\nexit /b 0\r\n",
    )
    .expect("payload tool should be written");
    let prepared = SafeFlashService::new()
        .resolve_source_with_cancellation(
            SafeFlashSource::Online {
                url: server.uri(),
                pd: "PD2057".to_string(),
                version: "16.2.10.0".to_string(),
                payload_dumper: Some(tool),
            },
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("online payload ZIP should be extracted through the controlled dumper");

    let staging = prepared
        .staging_root
        .expect("payload images should use Safe Flash staging");
    assert_eq!(prepared.partitions.len(), 1);
    assert!(prepared.partitions[0]
        .image_path
        .starts_with(staging.to_string_lossy().as_ref()));
    let after_staging = fs::read_dir(&safe_flash_parent)
        .expect("Safe Flash staging parent should be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<HashSet<_>>();
    let created_staging = after_staging
        .difference(&before_staging)
        .cloned()
        .collect::<Vec<_>>();
    assert!(created_staging.iter().any(|path| path == &staging));
    assert!(created_staging.iter().all(|path| {
        fs::read_dir(path)
            .expect("created staging should be readable")
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().ends_with("_ota.zip"))
    }));
    fs::remove_dir_all(staging).expect("payload staging should be removable");
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn payload_source_extracts_filtered_images_into_safe_flash_owned_staging() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-payload-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let tool = root.join("payload_dumper.cmd");
    fs::write(
        &tool,
        "@echo off\r\nset output=\r\nset metadata=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"--metadata\" set metadata=1\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\nif defined metadata ( >\"%output%\\metadata.json\" echo {\"partitions\":[{\"partition_name\":\"boot\",\"size_in_bytes\":9}]} ) else >\"%output%\\boot.img\" echo payload\r\nexit /b 0\r\n",
    )
    .expect("payload tool should be written");
    let payload = root.join("payload.bin");
    fs::write(&payload, b"CrAU").expect("payload fixture should be written");

    let prepared = SafeFlashService::new()
        .resolve_payload_source(
            &tool,
            &payload,
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        )
        .expect("payload should be extracted into Safe Flash staging");

    assert_eq!(prepared.partitions.len(), 1);
    assert_eq!(prepared.partitions[0].partition_name, "boot");
    let staging = prepared
        .staging_root
        .expect("payload staging should be owned");
    assert!(prepared.partitions[0]
        .image_path
        .starts_with(staging.to_string_lossy().as_ref()));
    assert!(!root.join("boot.img").exists());

    fs::remove_dir_all(staging).expect("payload staging should be removable");
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}

#[test]
fn payload_zip_extracts_its_payload_into_safe_flash_owned_staging_before_invoking_dumper() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-payload-zip-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let tool = root.join("payload_dumper.cmd");
    fs::write(
        &tool,
        "@echo off\r\nset source=%~1\r\nset output=\r\nset metadata=\r\necho %source%>\"%~dp0payload-source.txt\"\r\necho %source% | findstr /I /R \\\"\\.zip$\\\" >nul && exit /b 2\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"--metadata\" set metadata=1\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\nif defined metadata ( >\"%output%\\metadata.json\" echo {\"partitions\":[{\"partition_name\":\"boot\",\"size_in_bytes\":9}]} ) else >\"%output%\\boot.img\" echo payload\r\nexit /b 0\r\n",
    )
    .expect("payload tool should be written");
    let archive_path = root.join("firmware.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).expect("zip should be created"));
    archive
        .start_file("firmware/payload.bin", SimpleFileOptions::default())
        .expect("payload entry should be created");
    std::io::Write::write_all(&mut archive, b"CrAU-payload")
        .expect("payload entry should be written");
    archive.finish().expect("zip should be finalized");

    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_for_sink = progress.clone();
    let progress_sink: Arc<nwflash_application::SafeFlashPreparationProgressSink> =
        Arc::new(move |phase, completed, total| {
            progress_for_sink
                .lock()
                .expect("progress lock should be available")
                .push((phase, completed, total));
        });
    let prepared = SafeFlashService::new()
        .resolve_payload_source_with_cancellation_and_progress(
            &tool,
            &archive_path,
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: false,
                is_keep_root: false,
                wipe_data: false,
                wipe_data_image_path: None,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
            &CancellationToken::new(),
            Some(&progress_sink),
        )
        .expect("payload ZIP should be extracted into Safe Flash staging");

    let staging = prepared
        .staging_root
        .expect("payload ZIP staging should be owned");
    let dumper_source = fs::read_to_string(root.join("payload-source.txt"))
        .expect("payload dumper should receive a source path");
    let staged_payload = std::path::PathBuf::from(dumper_source.trim());
    assert!(staged_payload.starts_with(&staging));
    assert_eq!(
        staged_payload.file_name().and_then(|name| name.to_str()),
        Some("payload.bin")
    );
    assert_eq!(
        fs::read(&staged_payload).expect("staged payload should exist"),
        b"CrAU-payload"
    );
    assert!(!root.join("payload.bin").exists());
    let reported = progress.lock().expect("progress lock should be available");
    assert!(reported.iter().any(|(phase, completed, total)| {
        *phase == SafeFlashPreparationPhase::PayloadStaging
            && *completed > 0
            && *completed == *total
    }));
    assert!(reported
        .iter()
        .any(|(phase, _, _)| *phase == SafeFlashPreparationPhase::PayloadExtraction));
    assert!(prepared.partitions[0]
        .image_path
        .starts_with(staging.to_string_lossy().as_ref()));

    fs::remove_dir_all(staging).expect("payload ZIP staging should be removable");
    fs::remove_dir_all(root).expect("fixture directory should be removed");
}
