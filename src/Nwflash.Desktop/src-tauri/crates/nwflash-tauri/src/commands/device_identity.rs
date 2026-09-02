//! 共享设备身份读取：从 ADB 设备读取 PD（产品代号）与系统版本。
//! 供安全刷写（线刷）与 Vivo ROOT 云端 OTA 提取共用。
//! 读设备信息在 Rust 内部完成，浏览器不提交 serial，也不获得原始输出。

use nwflash_application::{OperationAdmissionState, OperationCoordinator};
use nwflash_domain::OperationKind;
use nwflash_windows::{
    device_transport::DeviceTransport,
    platform_tools::PlatformTools,
    process::{run_command, ProcessOutput},
    ProcessExecutor, SystemProcessExecutor,
};

use super::device::{admission_reason_from_domain_error, AdmissionCheckedExecutor};

pub async fn read_online_ota_identity(serial: &str) -> Result<(String, String), String> {
    let serial = serial.to_string();
    tokio::task::spawn_blocking(move || read_online_ota_identity_blocking(serial))
        .await
        .map_err(|_| "读取设备信息调度失败。".to_string())?
}

fn read_online_ota_identity_blocking(serial: String) -> Result<(String, String), String> {
    let transport = DeviceTransport::new(PlatformTools::bundled());
    let command = transport
        .build_adb_getprop_command(&serial)
        .map_err(|_| "无法构造设备信息读取请求。".to_string())?;
    let output = run_command(command).map_err(|_| "读取已连接设备的 PD/版本失败。".to_string())?;
    online_ota_identity_from_process_output(output)
}

fn read_online_ota_identity_blocking_with_executor<E: ProcessExecutor>(
    serial: String,
    executor: E,
) -> Result<(String, String), IdentityReadFailure> {
    let transport = DeviceTransport::new(PlatformTools::bundled());
    let command = transport
        .build_adb_getprop_command(&serial)
        .map_err(|_| IdentityReadFailure::Read)?;
    let output = executor.run(command).map_err(|error| {
        admission_reason_from_domain_error(&error)
            .map(IdentityReadFailure::Admission)
            .unwrap_or(IdentityReadFailure::Read)
    })?;
    online_ota_identity_from_process_output(output).map_err(|_| IdentityReadFailure::Read)
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
    let version = [
        value("ro.build.version.bbk").and_then(|bbk| bbk.rsplit('_').next()),
        value("ro.build.display.id"),
        value("ro.build.version.incremental"),
        value("ro.vivo.os.build.display.id"),
    ]
    .into_iter()
    .flatten()
    // A generic fallback value (`release-keys` / `unknown` / `not found`) is
    // not a real version; skip it and try the next candidate (the WPF
    // `IsGenericVersion` filter).
    .find(|candidate| !is_generic_version(candidate))
    .ok_or_else(|| "无法从已连接设备读取系统版本。".to_string())?;
    Ok((pd.to_string(), version.to_string()))
}

fn is_generic_version(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("release-keys") || lower == "unknown" || lower == "not found"
}

#[cfg(test)]
pub(crate) fn identity_refresh_is_blocked(
    admission: OperationAdmissionState,
    operation: OperationKind,
) -> bool {
    identity_refresh_block_reason(admission, operation).is_some()
}

pub(crate) fn identity_refresh_block_reason(
    admission: OperationAdmissionState,
    operation: OperationKind,
) -> Option<&'static str> {
    match admission {
        OperationAdmissionState::ExitPending => Some("skipped:exit_pending"),
        OperationAdmissionState::Terminating => Some("skipped:terminating"),
        OperationAdmissionState::Running if operation == OperationKind::Flashing => {
            Some("denied:flashing")
        }
        OperationAdmissionState::Running => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdentityReadFailure {
    Admission(&'static str),
    Scheduling,
    Read,
}

impl IdentityReadFailure {
    pub(crate) fn admission_reason(self) -> Option<&'static str> {
        match self {
            Self::Admission(reason) => Some(reason),
            Self::Scheduling | Self::Read => None,
        }
    }
}

async fn read_identity_if_admitted_with<BeforeSpawn, Read>(
    coordinator: &OperationCoordinator,
    before_spawn: BeforeSpawn,
    read: Read,
) -> Result<(String, String), IdentityReadFailure>
where
    BeforeSpawn: FnOnce() + Send + 'static,
    Read: FnOnce() -> Result<(String, String), IdentityReadFailure> + Send + 'static,
{
    let coordinator = coordinator.clone();
    match tokio::task::spawn_blocking(move || {
        before_spawn();
        if let Some(reason) =
            identity_refresh_block_reason(coordinator.admission_state(), OperationKind::Idle)
        {
            return Err(IdentityReadFailure::Admission(reason));
        }
        read()
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(IdentityReadFailure::Scheduling),
    }
}

/// Coordinator-aware identity reader for refresh paths. The admission state is
/// checked inside the blocking executor immediately before the ADB call. The
/// legacy reader above remains unchanged for safe-flash, which already owns an
/// authorized Flashing operation.
pub(crate) async fn read_identity_if_admitted(
    serial: &str,
    coordinator: &OperationCoordinator,
) -> Result<(String, String), IdentityReadFailure> {
    let executor = AdmissionCheckedExecutor::new(
        coordinator.clone(),
        OperationKind::Idle,
        SystemProcessExecutor,
    );
    read_identity_if_admitted_with_executor(serial, coordinator, executor).await
}

async fn read_identity_if_admitted_with_executor<E>(
    serial: &str,
    coordinator: &OperationCoordinator,
    executor: E,
) -> Result<(String, String), IdentityReadFailure>
where
    E: ProcessExecutor + Send + 'static,
{
    let serial = serial.to_string();
    read_identity_if_admitted_with(
        coordinator,
        || {},
        move || read_online_ota_identity_blocking_with_executor(serial, executor),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use nwflash_windows::{ProcessCommand, ProcessExecutor, ProcessOutput};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };

    #[test]
    fn getprop_parses_pd_and_bbk_version_tail() {
        let output = "\n\
            [ro.product.device]: [PD2417]\n\
            [ro.build.version.bbk]: [DPD2221B_A_16.2.12.0.W10.V000L1]\n\
            [ro.build.display.id]: [PD2417A_16.2.12]\n";
        let (pd, version) =
            online_ota_identity_from_getprop(output).expect("完整 bbk 行应解析出 PD 与版本末段");

        assert_eq!(pd, "PD2417");
        assert_eq!(version, "16.2.12.0.W10.V000L1");
    }

    #[test]
    fn getprop_skips_generic_version_values() {
        let output = "\n\
            [ro.product.device]: [PD2417]\n\
            [ro.build.version.bbk]: [DPD2221B_A_release-keys]\n\
            [ro.build.display.id]: [unknown]\n\
            [ro.build.version.incremental]: [16.2.12]\n";
        let (pd, version) =
            online_ota_identity_from_getprop(output).expect("generic 候选应被跳过，取到真实版本");

        assert_eq!(pd, "PD2417");
        assert_eq!(version, "16.2.12");
    }

    #[test]
    fn getprop_rejects_when_all_version_candidates_are_generic() {
        let output = "\n\
            [ro.product.device]: [PD2417]\n\
            [ro.build.display.id]: [unknown]\n\
            [ro.build.version.incremental]: [not found]\n";
        let error =
            online_ota_identity_from_getprop(output).expect_err("全部候选为 generic 时应失败");

        assert!(error.contains("系统版本"));
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

    #[test]
    fn identity_refresh_gate_skips_adb_spawn_during_flashing_or_teardown() {
        use nwflash_application::OperationAdmissionState;
        use nwflash_domain::OperationKind;

        assert_eq!(
            identity_refresh_block_reason(
                OperationAdmissionState::Running,
                OperationKind::Flashing,
            ),
            Some("denied:flashing")
        );
        assert_eq!(
            identity_refresh_block_reason(
                OperationAdmissionState::ExitPending,
                OperationKind::Idle,
            ),
            Some("skipped:exit_pending")
        );
        assert_eq!(
            identity_refresh_block_reason(
                OperationAdmissionState::Terminating,
                OperationKind::Idle,
            ),
            Some("skipped:terminating")
        );
        assert!(identity_refresh_is_blocked(
            OperationAdmissionState::Running,
            OperationKind::Flashing,
        ));
        assert!(identity_refresh_is_blocked(
            OperationAdmissionState::ExitPending,
            OperationKind::Idle,
        ));
        assert!(identity_refresh_is_blocked(
            OperationAdmissionState::Terminating,
            OperationKind::Idle,
        ));
        assert!(!identity_refresh_is_blocked(
            OperationAdmissionState::Running,
            OperationKind::Idle,
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_identity_spawn_rechecks_terminating_after_idle_admission() {
        let coordinator = nwflash_application::OperationCoordinator::default();
        let idle = coordinator
            .try_acquire_idle()
            .expect("initial identity admission should be idle");
        let reached_boundary = Arc::new(Barrier::new(2));
        let release_boundary = Arc::new(Barrier::new(2));
        let spawn_count = Arc::new(AtomicUsize::new(0));

        let task = tokio::spawn({
            let coordinator = coordinator.clone();
            let reached_boundary = reached_boundary.clone();
            let release_boundary = release_boundary.clone();
            let spawn_count = spawn_count.clone();
            async move {
                read_identity_if_admitted_with(
                    &coordinator,
                    move || {
                        reached_boundary.wait();
                        release_boundary.wait();
                    },
                    move || {
                        spawn_count.fetch_add(1, Ordering::SeqCst);
                        Ok(("PD2417".to_string(), "1.0".to_string()))
                    },
                )
                .await
            }
        });

        tokio::task::spawn_blocking(move || reached_boundary.wait())
            .await
            .expect("boundary waiter should finish");
        assert_eq!(
            coordinator.request_exit_pending(),
            OperationAdmissionState::ExitPending
        );
        coordinator
            .begin_terminating(&idle)
            .expect("idle teardown lease should enter terminating");
        tokio::task::spawn_blocking(move || release_boundary.wait())
            .await
            .expect("boundary release should finish");

        let error = task
            .await
            .expect("guarded identity task should join")
            .expect_err("terminating identity refresh must stop before executor spawn");
        assert_eq!(error.admission_reason(), Some("skipped:terminating"));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
    }

    #[derive(Clone)]
    struct CountingProcessExecutor {
        spawn_count: Arc<AtomicUsize>,
    }

    impl ProcessExecutor for CountingProcessExecutor {
        fn run(
            &self,
            _command: ProcessCommand,
        ) -> Result<ProcessOutput, nwflash_domain::DomainError> {
            self.spawn_count.fetch_add(1, Ordering::SeqCst);
            Ok(ProcessOutput {
                exit_code: 0,
                stdout: "[ro.product.device]: [PD2417]\n[ro.build.version.incremental]: [1.0]\n"
                    .to_string(),
                stderr: String::new(),
            })
        }
    }

    #[tokio::test]
    async fn actual_identity_executor_denial_remains_a_safe_admission_outcome() {
        let coordinator = OperationCoordinator::default();
        let _idle = coordinator
            .try_acquire_idle()
            .expect("initial identity admission should be idle");
        let spawn_count = Arc::new(AtomicUsize::new(0));
        let executor = AdmissionCheckedExecutor::with_hook(
            coordinator.clone(),
            CountingProcessExecutor {
                spawn_count: spawn_count.clone(),
            },
            {
                let coordinator = coordinator.clone();
                move || {
                    coordinator.request_exit_pending();
                }
            },
        );

        let error =
            read_identity_if_admitted_with_executor("SERIAL-PRIVATE", &coordinator, executor)
                .await
                .expect_err("actual identity executor denial must stay categorized");

        assert_eq!(error.admission_reason(), Some("skipped:exit_pending"));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);
        assert!(!format!("{error:?}").contains("SERIAL-PRIVATE"));
    }
}
