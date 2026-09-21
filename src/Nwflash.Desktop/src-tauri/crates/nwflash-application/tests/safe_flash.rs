use std::{
    collections::{HashSet, VecDeque},
    fs::{self, File},
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use nwflash_application::{
    SafeFlashBuildOptions, SafeFlashExecutionRequest, SafeFlashExecutionService,
    SafeFlashPartitionFailure, SafeFlashPartitionFailureDecision, SafeFlashPartitionSource,
    SafeFlashPreparationPhase, SafeFlashPreparedSource, SafeFlashService, SafeFlashSource,
    SAFE_FLASH_WIPE_DATA_MANUAL_STEPS,
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

/// 受保护分区：留在队列里做“假刷写”，`simulated_flash_bytes` 给出计时用的
/// 镜像大小（0 表示不等待，便于测试即时返回）。
fn simulated_partition(name: &str, bytes: u64) -> SafeFlashPartitionSource {
    SafeFlashPartitionSource {
        partition_name: name.to_string(),
        image_path: format!("C:\\staging\\{name}.img"),
        has_slot: true,
        simulated_flash_bytes: Some(bytes),
    }
}

/// 正常刷写分区。
fn real_partition(name: &str) -> SafeFlashPartitionSource {
    SafeFlashPartitionSource {
        partition_name: name.to_string(),
        image_path: format!("C:\\staging\\{name}.img"),
        has_slot: true,
        simulated_flash_bytes: None,
    }
}

fn dispatched_flash_targets(executor: &RecordedExecutor) -> Vec<String> {
    executor
        .commands()
        .into_iter()
        .filter(|command| command.args.get(2).is_some_and(|value| value == "flash"))
        .filter_map(|command| command.args.get(3).cloned())
        .collect()
}

fn common_partitions() -> Vec<SafeFlashPartitionSource> {
    vec![
        SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\tmp\\boot.img".to_string(),
            has_slot: true,
            simulated_flash_bytes: None,
        },
        SafeFlashPartitionSource {
            partition_name: "init_boot".to_string(),
            image_path: "C:\\tmp\\init_boot.img".to_string(),
            has_slot: true,
            simulated_flash_bytes: None,
        },
        SafeFlashPartitionSource {
            partition_name: "preloader".to_string(),
            image_path: "C:\\tmp\\preloader.img".to_string(),
            has_slot: true,
            simulated_flash_bytes: None,
        },
        SafeFlashPartitionSource {
            partition_name: "vendor_boot".to_string(),
            image_path: "C:\\tmp\\vendor_boot.img".to_string(),
            has_slot: true,
            simulated_flash_bytes: None,
        },
        SafeFlashPartitionSource {
            partition_name: "userdata".to_string(),
            image_path: "C:\\tmp\\userdata.img".to_string(),
            has_slot: true,
            simulated_flash_bytes: None,
        },
    ]
}

#[test]
fn safe_flash_build_plan_keeps_every_partition_in_the_queue() {
    // 受保护分区（lk/preloader 与勾选安全刷写后的系统分区）与保留 ROOT 的
    // 启动分区都留在计划里：它们仍会出现在刷写队列与日志中（假刷写），
    // 因此预检计数不能把它们排除掉。
    let service = make_service();
    let partitions = common_partitions();
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: true,
        is_keep_root: true,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: Some("a".to_string()),
    };

    let plan = service
        .build_plan(&partitions, options)
        .expect("safe flash plan should build");

    assert_eq!(
        plan.tasks
            .iter()
            .map(|task| task.partition_name.as_str())
            .collect::<Vec<_>>(),
        ["boot", "init_boot", "preloader", "vendor_boot", "userdata"]
    );
}

#[test]
fn execution_uses_the_sole_fastbootd_device_after_transition_and_flashes_every_partition() {
    // 分区存在性校验已移除：不再查询 partition-type，直接尝试刷写。
    // 勾选「清除数据」时队列以 `reboot recovery` 收尾（不再写 misc，也没有
    // 后续的普通 reboot）。
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("ADB-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) current-slot: a\n"),
        successful_output("(bootloader) has-slot:boot: yes\n"),
        successful_output("(bootloader) has-slot:vendor_boot: yes\n"),
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
                simulated_flash_bytes: None,
            },
            SafeFlashPartitionSource {
                partition_name: "vendor_boot".to_string(),
                image_path: "C:\\staging\\vendor_boot.img".to_string(),
                has_slot: true,
                simulated_flash_bytes: None,
            },
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "ADB-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: true,
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

    // boot_b + vendor_boot_b：两个分区刷写命令全部成功（清除数据不再写 misc，
    // 也不再计入刷写成功数）。
    assert_eq!(result.flashed_partition_count, 2);
    assert_eq!(result.skipped_partition_count, 0);
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
    // 队列末尾：对槽切换 + `reboot recovery`（不再有 misc 与普通 reboot）。
    assert_eq!(commands[commands.len() - 2].args[2], "set_active");
    assert_eq!(
        commands
            .last()
            .expect("reboot recovery command expected")
            .args[2..],
        ["reboot", "recovery"]
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
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    let error = service
        .execute(
            request_for_other_slot(&source, &options, false),
            || false,
            |_| {},
            |_| {},
        )
        .expect_err("bootloader fastboot must not be accepted as fastbootd");

    assert!(error.to_string().contains("fastbootd"));
    assert!(!executor
        .commands()
        .iter()
        .any(|command| command.args.get(2) == Some(&"flash".to_string())));
}

#[test]
fn execution_never_flashes_another_device_that_appears_in_fastbootd() {
    // 跨设备防护（N3/N8）：计划设备是 FASTBOOT-001，等待期间出现的是
    // FASTBOOT-002（另一台在 fastboot 的手机）。绝不能把 A 机修补的镜像
    // 刷进 B 机——必须一直等待目标设备，等待耗尽后明确超时失败。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-002\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(2, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:////staging////boot.img".to_string(),
            has_slot: true,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    let error = service
        .execute(
            request_for_other_slot(&source, &options, false),
            || false,
            |_| {},
            |_| {},
        )
        .expect_err("another device in fastbootd must never receive the planned images");

    assert!(
        error.to_string().contains("FASTBOOT-001"),
        "timeout must name the planned device, got: {error}"
    );
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
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
fn execution_degrades_to_partition_original_name_when_current_slot_is_unreadable() {
    // getvar 瞬态抖动时的安全降级（C# SafeFlashSlotPlanner 语义）：
    // current-slot 读不到按非 A/B 处理，目标回退分区原名继续刷写，
    // 不追加 set_active，而不是放弃整次刷写。
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: current-slot unavailable)".to_string(),
        }),
        successful_output("(bootloader) has-slot:boot: yes\n"),
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
            has_slot: true,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    service
        .execute(
            request_for_other_slot(&source, &options, true),
            || false,
            |_| {},
            |_| {},
        )
        .expect("an unreadable current slot must degrade to the original partition name");

    // 刷写目标必须是分区原名 boot（无槽位后缀），且不追加 set_active。
    assert!(executor.commands().iter().any(|command| {
        command.args.get(2) == Some(&"flash".to_string())
            && command.args.get(3) == Some(&"boot".to_string())
    }));
    assert!(!executor
        .commands()
        .iter()
        .any(|command| command.args.contains(&"set_active".to_string())));
}

#[test]
fn execution_degrades_to_partition_original_name_when_has_slot_is_unreadable() {
    // has-slot 读不到按 false 处理（C# HasSlotAsync 查询失败回退原样刷写），
    // 瞬态 getvar 失败不放弃整次刷写。
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
        successful_output(""),
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
            has_slot: true,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    service
        .execute(
            request_for_other_slot(&source, &options, true),
            || false,
            |_| {},
            |_| {},
        )
        .expect("an unreadable has-slot fact must degrade to the original partition name");

    assert!(executor.commands().iter().any(|command| {
        command.args.get(2) == Some(&"flash".to_string())
            && command.args.get(3) == Some(&"boot".to_string())
    }));
    // current-slot 可读(a)时 OtherSlot 仍追加 set_active b:has_slot=false
    // 只把刷写目标降级为分区原名,不取消槽位切换。
    assert!(executor.commands().iter().any(|command| {
        command.args.get(2) == Some(&"set_active".to_string())
            && command.args.get(3) == Some(&"b".to_string())
    }));
}

#[test]
fn execution_degrades_to_partition_original_name_when_has_slot_is_unrecognized() {
    // 无法识别的 has-slot 值同样按 false 降级（C# 回退原样刷写）。
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output("(bootloader) current-slot: a\n"),
        successful_output("(bootloader) has-slot:boot: unknown\n"),
        successful_output(""),
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
            has_slot: true,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::OtherSlot,
        current_slot: None,
    };

    service
        .execute(
            request_for_other_slot(&source, &options, true),
            || false,
            |_| {},
            |_| {},
        )
        .expect("an unrecognized has-slot value must degrade to the original partition name");

    assert!(executor.commands().iter().any(|command| {
        command.args.get(2) == Some(&"flash".to_string())
            && command.args.get(3) == Some(&"boot".to_string())
    }));
}

#[test]
fn execution_uses_the_current_target_when_it_differs_from_preflight_target() {
    let executor = RecordedExecutor::new([
        successful_output("CURRENT-FASTBOOT\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
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
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "PRECHECK-STALE".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
fn execution_never_flashes_a_different_device_after_adb_to_fastbootd_transition() {
    // N3 回归：ADB 阶段是 ADB-001，重启进 fastbootd 后出现的是 OTHER-DEVICE
    // （另一台手机，或 A 机没起来）。旧行为会把 A 机修补的镜像刷进 OTHER-DEVICE。
    // 对齐 C# `expectedSerial`：只接受同一台设备，不一致即等待直至超时并失败。
    let executor = RecordedExecutor::new([
        successful_output(""),
        successful_output("OTHER-DEVICE\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "init_boot".to_string(),
            image_path: "C:////staging////init_boot.img".to_string(),
            has_slot: false,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "ADB-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
        .expect_err("a different device must never receive the planned images");

    assert!(
        error.to_string().contains("ADB-001"),
        "failure must name the planned device, got: {error}"
    );
    let commands = executor.commands();
    assert_eq!(commands[0].args, ["-s", "ADB-001", "reboot", "fastboot"]);
    assert!(!commands
        .iter()
        .any(|command| command.args.get(2) == Some(&"flash".to_string())));
    assert!(!commands
        .iter()
        .any(
            |command| command.args.get(1).map(String::as_str) == Some("OTHER-DEVICE")
                && command.args.len() > 2
        ));
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
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "192.168.1.2:5555".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "ADB-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
                simulated_flash_bytes: None,
            },
            SafeFlashPartitionSource {
                partition_name: "vendor_boot".to_string(),
                image_path: "C:\\staging\\vendor_boot.img".to_string(),
                has_slot: true,
                simulated_flash_bytes: None,
            },
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
    // 无决策回调时保持统一脱敏文案（与 HEAD 语义一致）：不携带 fastboot
    // 原始输出，退出码详情只进入决策回调的弹窗日志。
    assert!(message.contains("fastboot 命令执行失败"));
    assert!(!message.contains("FASTBOOT-SECRET"));
    assert!(!message.contains("secret"));
    assert!(!message.contains("rom.invalid"));
    let commands = executor.commands();
    assert_eq!(commands.len(), 3);
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
fn partition_failure_hook_carries_partition_name_and_fastboot_log() {
    // 分区失败回调必须带回分区名和 fastboot 原始报错（供弹窗展示），
    // 但不含镜像路径等敏感信息（错误信息由 fastboot_failure_summary 从
    // stdout/stderr 重建，路径不进决策回调）。
    let executor = RecordedExecutor::new([
        // CurrentSlot + 单分区（无 has-slot/current-slot 探测）：
        // devices → is-userspace → flash boot（失败）。
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: 'partition write failed')".to_string(),
        }),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: false,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let observed: Mutex<Vec<SafeFlashPartitionFailure>> = Mutex::new(Vec::new());

    let error = service
        .execute_with_partition_failure_hook(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
            Some(|failure| {
                observed
                    .lock()
                    .expect("observed failures lock should not be poisoned")
                    .push(failure);
                Ok(SafeFlashPartitionFailureDecision::Abort)
            }),
        )
        .expect_err("abort decision must stop the workflow");

    assert!(matches!(error, DomainError::UserCancelled(_)));
    let observed = observed
        .into_inner()
        .expect("observed failures lock should not be poisoned");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].partition_name, "boot");
    assert!(
        observed[0].error_message.contains("partition write failed"),
        "failure log must include the raw fastboot error: {}",
        observed[0].error_message
    );
    assert!(!observed[0].error_message.contains("boot.img"));
    // 中止后不能执行任何 reboot。
    assert!(!executor.commands().iter().any(|command| command
        .args
        .get(2)
        .is_some_and(|argument| argument == "reboot")));
}

#[test]
fn partition_failure_hook_does_not_intercept_user_cancellation() {
    // 执行器在分区命令处返回 UserCancelled 时，不应被包装成分区失败
    // 决策，也不应在取消收尾阶段追加 reboot。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        Err(DomainError::UserCancelled("底层进程已停止".to_string())),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: false,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let observed: Mutex<Vec<SafeFlashPartitionFailure>> = Mutex::new(Vec::new());

    let error = service
        .execute_with_partition_failure_hook(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
            Some(|failure| {
                observed
                    .lock()
                    .expect("observed failures lock should not be poisoned")
                    .push(failure);
                Ok(SafeFlashPartitionFailureDecision::Abort)
            }),
        )
        .expect_err("user cancellation must stop execution");

    assert!(matches!(error, DomainError::UserCancelled(_)));
    assert!(observed
        .into_inner()
        .expect("observed failures lock should not be poisoned")
        .is_empty());
    assert!(!executor
        .commands()
        .iter()
        .any(|command| command.args.iter().any(|argument| argument == "reboot")));
}

#[test]
fn partition_failure_continue_decision_flashes_remaining_partitions_and_reboots() {
    // 用户选择“继续刷写”：失败的分区跳过，其余分区、（无清除数据时）
    // 尾部 reboot 照常执行。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: 'boot write refused')".to_string(),
        }),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            SafeFlashPartitionSource {
                partition_name: "boot".to_string(),
                image_path: "C:\\staging\\boot.img".to_string(),
                has_slot: false,
                simulated_flash_bytes: None,
            },
            SafeFlashPartitionSource {
                partition_name: "vendor_boot".to_string(),
                image_path: "C:\\staging\\vendor_boot.img".to_string(),
                has_slot: false,
                simulated_flash_bytes: None,
            },
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let result = service
        .execute_with_partition_failure_hook(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
            Some(|_failure| Ok(SafeFlashPartitionFailureDecision::Continue)),
        )
        .expect("continue decision should finish the remaining queue");

    assert_eq!(result.flashed_partition_count, 1);
    assert_eq!(result.skipped_partition_count, 1);
    let commands = executor.commands();
    // 命令日志包含失败尝试本身（boot 的 flash 已发出但失败被跳过），
    // 这里断言队列在失败分区之后仍继续执行了 vendor_boot 与收尾 reboot。
    let flashed_partitions = commands
        .iter()
        .filter(|command| {
            command
                .args
                .get(2)
                .is_some_and(|argument| argument == "flash")
        })
        .filter_map(|command| command.args.get(3))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(flashed_partitions, ["boot", "vendor_boot"]);
    assert!(commands
        .last()
        .expect("reboot expected")
        .args
        .iter()
        .any(|argument| argument == "reboot"));
}

#[test]
fn partition_failure_retry_decision_retries_the_same_partition_without_advancing_progress() {
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: 'transient write failure')".to_string(),
        }),
        successful_output(""),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            SafeFlashPartitionSource {
                partition_name: "boot".to_string(),
                image_path: "C:\\staging\\boot.img".to_string(),
                has_slot: false,
                simulated_flash_bytes: None,
            },
            SafeFlashPartitionSource {
                partition_name: "vendor_boot".to_string(),
                image_path: "C:\\staging\\vendor_boot.img".to_string(),
                has_slot: false,
                simulated_flash_bytes: None,
            },
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let progress = Mutex::new(Vec::new());
    let failures = Mutex::new(Vec::new());

    let result = service
        .execute_with_partition_failure_hook(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |value| {
                progress
                    .lock()
                    .expect("progress lock should not be poisoned")
                    .push(value)
            },
            Some(|failure| {
                failures
                    .lock()
                    .expect("failures lock should not be poisoned")
                    .push(failure);
                Ok(SafeFlashPartitionFailureDecision::Retry)
            }),
        )
        .expect("a successful retry should finish the workflow");

    assert_eq!(result.command_count, 3);
    assert_eq!(result.executed_command_count, 3);
    assert_eq!(result.flashed_partition_count, 2);
    assert_eq!(result.skipped_partition_count, 0);
    assert_eq!(
        *progress
            .lock()
            .expect("progress lock should not be poisoned"),
        [1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]
    );
    let failures = failures
        .into_inner()
        .expect("failures lock should not be poisoned");
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].partition_name, "boot");

    let flash_targets = executor
        .commands()
        .into_iter()
        .filter(|command| command.args.get(2).is_some_and(|value| value == "flash"))
        .filter_map(|command| command.args.get(3).cloned())
        .collect::<Vec<_>>();
    assert_eq!(flash_targets, ["boot", "boot", "vendor_boot"]);
}

#[test]
fn wipe_data_queues_a_recovery_reboot_as_the_last_step() {
    // 勾选「清除数据」：队列末尾是 `fastboot reboot recovery`——既不写 misc，
    // 也没有后续的普通 reboot（设备已经离开 fastboot）。日志必须给出进 REC
    // 后的手动清除步骤。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
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
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: true,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let result = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |stage| {
                stages
                    .lock()
                    .expect("stages lock should not be poisoned")
                    .push(stage)
            },
            |_| {},
        )
        .expect("wipe-data flow should complete");

    assert_eq!(result.flashed_partition_count, 1);
    let commands = executor.commands();
    assert!(!commands
        .iter()
        .any(|command| command.args.iter().any(|argument| argument == "misc")));
    assert_eq!(
        commands
            .last()
            .expect("reboot recovery command expected")
            .args[2..],
        ["reboot", "recovery"]
    );
    let stages = stages
        .into_inner()
        .expect("stages lock should not be poisoned");
    assert!(
        stages.contains(&SAFE_FLASH_WIPE_DATA_MANUAL_STEPS.to_string()),
        "日志必须给出手动清除步骤：{stages:?}"
    );
}

#[test]
fn reboot_recovery_failure_is_reported_without_failing_the_workflow() {
    // `reboot recovery` 是可容忍的收尾动作：机型不支持该目标时设备仍停在
    // fastbootd，用户手动进 REC 同样能清数据，因此只提示、不中止、不进
    // 分区失败弹窗。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output(""),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: 'unknown reboot target')".to_string(),
        }),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: false,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: true,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let observed: Mutex<Vec<SafeFlashPartitionFailure>> = Mutex::new(Vec::new());
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let result = service
        .execute_with_partition_failure_hook(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |stage| {
                stages
                    .lock()
                    .expect("stages lock should not be poisoned")
                    .push(stage)
            },
            |_| {},
            Some(|failure| {
                observed
                    .lock()
                    .expect("observed failures lock should not be poisoned")
                    .push(failure);
                Ok(SafeFlashPartitionFailureDecision::Continue)
            }),
        )
        .expect("a failed recovery reboot must not fail the whole flash");

    assert_eq!(result.flashed_partition_count, 1);
    // 队列只有 [flash boot, reboot recovery]：前者成功计入，被容忍的后者不计。
    assert_eq!(result.command_count, 2);
    assert_eq!(result.executed_command_count, 1);
    assert!(observed
        .into_inner()
        .expect("observed failures lock should not be poisoned")
        .is_empty());
    let stages = stages
        .into_inner()
        .expect("stages lock should not be poisoned");
    assert!(
        stages
            .iter()
            .any(|stage| stage.contains("未能自动重启到REC")),
        "失败必须给出兜底提示：{stages:?}"
    );
}

#[test]
fn partition_failure_hook_ignores_failures_of_control_commands() {
    // 收尾 reboot（非分区刷写）失败仍按原语义直接中止，不进分区失败弹窗。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output(""),
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "FAILED (remote: 'reboot refused')".to_string(),
        }),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: false,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let observed: Mutex<Vec<SafeFlashPartitionFailure>> = Mutex::new(Vec::new());

    let error = service
        .execute_with_partition_failure_hook(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |_| {},
            |_| {},
            Some(|failure| {
                observed
                    .lock()
                    .expect("observed failures lock should not be poisoned")
                    .push(failure);
                Ok(SafeFlashPartitionFailureDecision::Continue)
            }),
        )
        .expect_err("control command failure must stop the workflow");

    assert!(error.to_string().contains("fastboot 命令执行失败"));
    assert!(observed
        .into_inner()
        .expect("observed failures lock should not be poisoned")
        .is_empty());
    // reboot 就是最后一条命令，失败后没有追加任何动作。
    assert_eq!(
        executor
            .commands()
            .last()
            .expect("reboot command expected")
            .args[2..],
        ["reboot"]
    );
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
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()));
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![SafeFlashPartitionSource {
            partition_name: "boot".to_string(),
            image_path: "C:\\staging\\boot.img".to_string(),
            has_slot: true,
            simulated_flash_bytes: None,
        }],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
    // 取消在前，因此只留下了 fastbootd 探测类命令：没有任何 flash，
    // 也没有收尾 reboot（分区存在性校验已删除，这里曾经还会有一条
    // getvar partition-type:boot）。
    let commands = executor.commands();
    assert_eq!(commands.len(), 2);
    assert!(!commands
        .iter()
        .any(|command| command.args.iter().any(|argument| argument == "flash")));
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
            simulated_flash_bytes: None,
        },
        SafeFlashPartitionSource {
            partition_name: "lk".to_string(),
            image_path: "C:\\tmp\\lk.img".to_string(),
            has_slot: false,
            simulated_flash_bytes: None,
        },
        SafeFlashPartitionSource {
            partition_name: "vbmeta".to_string(),
            image_path: "C:\\tmp\\vbmeta.img".to_string(),
            has_slot: false,
            simulated_flash_bytes: None,
        },
    ];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
        simulated_flash_bytes: None,
    }];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
fn safe_flash_build_plan_ignores_the_wipe_data_flag() {
    // 清除数据不再产生刷写任务（现在是收尾的 `reboot recovery`），
    // 勾不勾选都不影响计划里的分区清单与顺序。
    let service = make_service();
    let partitions = vec![SafeFlashPartitionSource {
        partition_name: "boot".to_string(),
        image_path: "C:\\tmp\\boot.img".to_string(),
        has_slot: true,
        simulated_flash_bytes: None,
    }];

    for wipe_data in [false, true] {
        let plan = service
            .build_plan(
                &partitions,
                SafeFlashBuildOptions {
                    serial: "SN-001".to_string(),
                    is_safe_flash: false,
                    is_keep_root: false,
                    wipe_data,
                    slot_mode: SafeFlashSlotMode::CurrentSlot,
                    current_slot: Some("a".to_string()),
                },
            )
            .expect("safe flash plan should build");
        assert_eq!(
            plan.tasks
                .iter()
                .map(|task| task.partition_name.as_str())
                .collect::<Vec<_>>(),
            ["boot"],
            "wipe_data={wipe_data}"
        );
    }
}

#[test]
fn safe_flash_commands_share_quick_flash_transport() {
    let service = make_service();
    let partitions = vec![SafeFlashPartitionSource {
        partition_name: "boot".to_string(),
        image_path: "C:\\tmp\\boot.img".to_string(),
        has_slot: true,
        simulated_flash_bytes: None,
    }];
    let options = SafeFlashBuildOptions {
        serial: "SN-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
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
async fn protected_partitions_are_extracted_like_a_real_flash() {
    // “假戏真做”：勾选安全刷写后，system 这类只做假刷写的分区同样要真的解包，
    // 产物落在 staging 里、大小与压缩包记录一致——预检阶段的解包量与占用与
    // 真机刷写毫无区别，唯一被模拟的是「写进设备」那一步。
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-protected-extract-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let archive_path = root.join("firmware.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).expect("zip should be created"));
    for (name, bytes) in [
        ("images/system.img", b"system-image-payload".as_slice()),
        ("images/userdata.img", b"userdata".as_slice()),
    ] {
        archive
            .start_file(name, SimpleFileOptions::default())
            .expect("zip entry should be created");
        std::io::Write::write_all(&mut archive, bytes).expect("zip image should be written");
    }
    archive.finish().expect("zip should be finalized");

    let prepared = SafeFlashService::new()
        .resolve_source(
            SafeFlashSource::LocalPath {
                path: archive_path.to_string_lossy().into_owned(),
            },
            &SafeFlashBuildOptions {
                serial: "SN-001".to_string(),
                is_safe_flash: true,
                is_keep_root: false,
                wipe_data: false,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        )
        .await
        .expect("protected partitions must still be extracted into staging");

    let staging_root = prepared
        .staging_root
        .clone()
        .expect("zip extraction must own a private staging root");
    assert_eq!(
        prepared
            .partitions
            .iter()
            .map(|source| source.partition_name.as_str())
            .collect::<Vec<_>>(),
        ["system", "userdata"]
    );

    let system = prepared
        .partitions
        .iter()
        .find(|source| source.partition_name == "system")
        .expect("system partition expected");
    let extracted_bytes = fs::metadata(&system.image_path)
        .expect("只做假刷写的分区同样必须真的解包落盘")
        .len();
    assert_eq!(extracted_bytes, b"system-image-payload".len() as u64);
    assert_eq!(system.simulated_flash_bytes, Some(extracted_bytes));
    assert!(system
        .image_path
        .starts_with(staging_root.to_string_lossy().as_ref()));

    let userdata = prepared
        .partitions
        .iter()
        .find(|source| source.partition_name == "userdata")
        .expect("userdata partition expected");
    assert_eq!(userdata.simulated_flash_bytes, None);
    assert_eq!(
        fs::metadata(&userdata.image_path)
            .expect("普通分区照旧落盘")
            .len(),
        b"userdata".len() as u64
    );

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
    // 只检查本次调用产生的 staging。`<temp>/nwflash-safe-flash` 是**全局共享**父目录，
    // 并发跑的其它用例会在两次 read_dir 之间创建并删除自己的 staging；遍历
    // 「新出现的目录」会撞上已经消失的兄弟目录（os error 3 / NotFound）。
    // `_ota.zip` 本来就落在自己的 staging 根里（safe_flash.rs 的
    // build_download_target_path(&staging_root, ...)），查自己即可。
    assert!(fs::read_dir(&staging)
        .expect("created staging should be readable")
        .filter_map(Result::ok)
        .all(|entry| !entry.file_name().to_string_lossy().ends_with("_ota.zip")));
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
fn payload_extracts_protected_partitions_and_marks_only_them_as_simulated() {
    // “假戏真做”在 payload 通道同样成立：勾选安全刷写后 system 也要真的解包，
    // 只是被标记为「只做假刷写」；boot 照旧解包并真刷。
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be available")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("nwflash-safe-flash-payload-protected-{nonce}"));
    fs::create_dir_all(&root).expect("fixture directory should be created");
    let tool = root.join("payload_dumper.cmd");
    fs::write(
        &tool,
        "@echo off\r\nset output=\r\nset metadata=\r\n:next\r\nif \"%~1\"==\"\" goto done\r\nif \"%~1\"==\"--metadata\" set metadata=1\r\nif \"%~1\"==\"-o\" set output=%~2\r\nshift\r\ngoto next\r\n:done\r\nif not defined metadata goto extract\r\n>\"%output%\\metadata.json\" echo {\"partitions\":[{\"partition_name\":\"system\",\"size_in_bytes\":8},{\"partition_name\":\"boot\",\"size_in_bytes\":6}]}\r\nexit /b 0\r\n:extract\r\n>\"%output%\\system.img\" echo system\r\n>\"%output%\\boot.img\" echo boot\r\nexit /b 0\r\n",
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
                is_safe_flash: true,
                is_keep_root: false,
                wipe_data: false,
                slot_mode: SafeFlashSlotMode::CurrentSlot,
                current_slot: None,
            },
        )
        .expect("payload should be extracted into Safe Flash staging");

    assert_eq!(prepared.partitions.len(), 2);
    let system = prepared
        .partitions
        .iter()
        .find(|source| source.partition_name == "system")
        .expect("system partition expected");
    assert_eq!(
        fs::read(&system.image_path).expect("受保护分区必须真的解包落盘"),
        b"system\r\n"
    );
    assert_eq!(system.simulated_flash_bytes, Some(8));

    let boot = prepared
        .partitions
        .iter()
        .find(|source| source.partition_name == "boot")
        .expect("boot partition expected");
    assert_eq!(fs::read(&boot.image_path).expect("普通分区照旧落盘"), b"boot\r\n");
    assert_eq!(boot.simulated_flash_bytes, None);

    let staging = prepared
        .staging_root
        .expect("payload staging should be owned");
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

#[test]
fn protected_partitions_are_reported_as_flashed_but_never_written_to_the_device() {
    // 勾选“安全刷写”：lk、preloader 与八个系统分区全部留在刷写队列里，
    // 日志逐条显示「刷写分区[i/n] ... OK」、计数与真实刷写完全一致，
    // 但执行器一条 fastboot flash 都收不到。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            simulated_partition("lk", 0),
            simulated_partition("preloader", 0),
            simulated_partition("system_a", 0),
            real_partition("userdata"),
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: true,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let result = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |stage| {
                stages
                    .lock()
                    .expect("stages lock should not be poisoned")
                    .push(stage)
            },
            |_| {},
        )
        .expect("protected partitions must not fail the workflow");

    assert_eq!(result.flashed_partition_count, 4);
    assert_eq!(result.command_count, 5);
    assert_eq!(result.executed_command_count, 5);
    assert_eq!(result.skipped_partition_count, 0);

    let stages = stages
        .into_inner()
        .expect("stages lock should not be poisoned");
    assert_eq!(
        stages
            .iter()
            .filter(|stage| stage.ends_with("OK"))
            .count(),
        4,
        "每条分区都必须是普通刷写日志：{stages:?}"
    );
    // 日志里不允许出现任何暗示“没有真的刷”的字样。
    assert!(stages.iter().all(|stage| !stage.contains("假")
        && !stage.contains("模拟")
        && !stage.contains("跳过")), "{stages:?}");

    assert_eq!(dispatched_flash_targets(&executor), ["userdata"]);
    // 分区存在性校验已删除：整条链路不再出现 getvar partition-type。
    assert!(!executor.commands().iter().any(|command| command
        .args
        .iter()
        .any(|argument| argument.contains("partition-type"))));
}

#[test]
fn without_safe_flash_only_lk_and_preloader_are_kept_off_the_device() {
    // 未勾选“安全刷写”：lk/preloader 依旧不写设备，而八个系统分区照常
    // 真实刷入。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output(""),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            simulated_partition("lk", 0),
            simulated_partition("preloader", 0),
            real_partition("system"),
            real_partition("userdata"),
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let result = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |stage| {
                stages
                    .lock()
                    .expect("stages lock should not be poisoned")
                    .push(stage)
            },
            |_| {},
        )
        .expect("unprotected system partitions must be written normally");

    // 四个分区全都计入刷写成功——包括两个没有真正写盘的受保护分区。
    assert_eq!(result.flashed_partition_count, 4);
    assert_eq!(
        dispatched_flash_targets(&executor),
        ["system", "userdata"],
        "只有 lk/preloader 被排除在真实写入之外"
    );
    let stages = stages
        .into_inner()
        .expect("stages lock should not be poisoned");
    assert_eq!(
        stages
            .iter()
            .filter(|stage| stage.ends_with("OK"))
            .count(),
        4,
        "{stages:?}"
    );
}

#[test]
fn keep_root_partitions_are_reported_as_flashed_but_never_written() {
    // 勾选“保留 ROOT”：boot / init_boot / vendor_boot 以及带槽位后缀的
    // boot_a 都留在刷写队列里、照常报 OK，但一条 fastboot flash 都不派发
    // （否则当前槽的 boot 会被覆盖，保留 ROOT 就失效了）。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            real_partition("boot"),
            real_partition("boot_a"),
            real_partition("userdata"),
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: true,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());

    let result = service
        .execute(
            SafeFlashExecutionRequest {
                source: &source,
                options: &options,
                serial: options.serial.as_str(),
                transition_to_fastbootd: false,
            },
            || false,
            |stage| {
                stages
                    .lock()
                    .expect("stages lock should not be poisoned")
                    .push(stage)
            },
            |_| {},
        )
        .expect("keep-root partitions must not fail the workflow");

    assert_eq!(result.flashed_partition_count, 3);
    assert_eq!(result.skipped_partition_count, 0);
    assert_eq!(dispatched_flash_targets(&executor), ["userdata"]);
    let stages = stages
        .into_inner()
        .expect("stages lock should not be poisoned");
    assert_eq!(
        stages
            .iter()
            .filter(|stage| stage.ends_with("OK"))
            .count(),
        3,
        "三个分区都要报出与真实刷写一致的完成日志：{stages:?}"
    );
}

#[test]
fn without_keep_root_boot_partitions_are_written_normally() {
    // 不勾选“保留 ROOT”时 boot 必须真的写进去（对照组，防止判定过宽）。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
        successful_output(""),
        successful_output(""),
        successful_output(""),
        successful_output(""),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![
            real_partition("boot"),
            real_partition("init_boot"),
            real_partition("vendor_boot"),
        ],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: false,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };

    let result = service
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
        .expect("boot partitions must be written when keep-root is off");

    assert_eq!(result.flashed_partition_count, 3);
    assert_eq!(
        dispatched_flash_targets(&executor),
        ["boot", "init_boot", "vendor_boot"]
    );
}

#[test]
fn simulated_flash_wait_stops_immediately_when_canceled() {
    // 350MB 按 35MB/s 需要 10 秒；取消后必须立刻收尾，而不是等完。
    let executor = RecordedExecutor::new([
        successful_output("FASTBOOT-001\tfastboot\n"),
        successful_output("(bootloader) is-userspace: yes\n"),
    ]);
    let service = SafeFlashExecutionService::new(Arc::new(executor.clone()))
        .with_fastbootd_wait(1, std::time::Duration::ZERO);
    let source = SafeFlashPreparedSource {
        staging_root: None,
        partitions: vec![simulated_partition("system", 350 * 1024 * 1024)],
        has_block_based_content: false,
    };
    let options = SafeFlashBuildOptions {
        serial: "FASTBOOT-001".to_string(),
        is_safe_flash: true,
        is_keep_root: false,
        wipe_data: false,
        slot_mode: SafeFlashSlotMode::CurrentSlot,
        current_slot: None,
    };
    let mut cancellation_checks = 0usize;
    let started = std::time::Instant::now();

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
                // 前 8 次检查发生在进入假刷写之前，第 9 次起落在等待切片里。
                cancellation_checks >= 11
            },
            |_| {},
            |_| {},
        )
        .expect_err("canceled simulated flash must stop the workflow");

    assert!(matches!(error, DomainError::UserCancelled(_)));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "取消后仍等待了 {:?}，说明假刷写等待不可中断",
        started.elapsed()
    );
    let commands = executor.commands();
    assert_eq!(commands.len(), 2);
    assert!(!commands
        .iter()
        .any(|command| command.args.iter().any(|argument| argument == "flash")));
}
