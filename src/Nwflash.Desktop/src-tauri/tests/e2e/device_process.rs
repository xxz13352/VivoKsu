use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use nwflash_domain::DomainError;
use nwflash_windows::{
    PlatformDeviceDiscovery, PlatformTools, ProcessCommand, ProcessExecutor, ProcessOutput,
};

#[derive(Clone)]
struct ScriptedProcessExecutor {
    outcomes: Arc<Mutex<VecDeque<Result<ProcessOutput, DomainError>>>>,
    commands: Arc<Mutex<Vec<ProcessCommand>>>,
}

impl ScriptedProcessExecutor {
    fn new(outcomes: Vec<Result<ProcessOutput, DomainError>>) -> Self {
        Self {
            outcomes: Arc::new(Mutex::new(outcomes.into())),
            commands: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn commands(&self) -> Vec<ProcessCommand> {
        self.commands.lock().expect("commands lock").clone()
    }
}

impl ProcessExecutor for ScriptedProcessExecutor {
    fn run(&self, command: ProcessCommand) -> Result<ProcessOutput, DomainError> {
        self.commands.lock().expect("commands lock").push(command);
        self.outcomes
            .lock()
            .expect("outcomes lock")
            .pop_front()
            .expect("a scripted process outcome should be available")
    }
}

#[test]
fn mocked_adb_failure_and_fastboot_recovery_keep_fixed_discovery_arguments() {
    let executor = ScriptedProcessExecutor::new(vec![
        Ok(ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: "adb server unavailable".to_string(),
        }),
        Ok(ProcessOutput {
            exit_code: 0,
            stdout: "FAST-1\tfastboot\n".to_string(),
            stderr: String::new(),
        }),
    ]);
    let discovery = PlatformDeviceDiscovery::with_executor(
        PlatformTools::new("adb.exe", "fastboot.exe"),
        executor.clone(),
    );

    let adb_error = discovery
        .discover_adb()
        .expect_err("the injected adb failure should be visible");
    assert!(adb_error.to_string().contains("adb server unavailable"));
    assert_eq!(
        discovery
            .discover_fastboot()
            .expect("fastboot recovery fixture should succeed"),
        "FAST-1\tfastboot\n"
    );

    let commands = executor.commands();
    assert_eq!(commands[0].args, vec!["devices", "-l"]);
    assert_eq!(commands[1].args, vec!["devices"]);
    assert_eq!(
        commands[1].environment,
        vec![("ADB".to_string(), "adb.exe".to_string())]
    );
}
