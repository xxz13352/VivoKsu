//! 反调试检测与「两段式」处置状态机。
//!
//! ## 底线（本模块存在的理由）
//!
//! 这是一个**本地刷机工具**。用户在写入分区的中途，设备可能正处于
//! fastboot 会话里。此时若因为"检测到调试器"就 `panic` 或 `exit`，进程
//! 会带着未完成的写入消失——**设备会变砖**。因此本模块有一条不可协商的
//! 规则：
//!
//! > **刷机进行中检测到调试器，绝不允许 panic / exit / 中止设备会话。
//! > 只能挂起并等待用户处置。**
//!
//! 这与项目里既有的**完整性终局**（`exit_supervisor` 的 `IntegrityReason`）
//! 是两条完全不同的路径，不能混用：
//!
//! | | 触发条件 | 处置 |
//! |---|---|---|
//! | 完整性终局 | 镜像 CRC 被篡改 / 租约签名无效 | 立即退出（镜像已不可信，继续写入才会变砖） |
//! | 反调试挂起（本模块） | 存在调试器 | **挂起等待**（调试器不改变镜像正确性，中断写入才危险） |
//!
//! ## 与 VMP 探针的关系
//!
//! 项目已通过 `nwflash_protection::VmpIntegrityProbe` 暴露 VMProtect 的
//! `VMProtectIsDebuggerPresent`，且 `packaging/vmprotect/README.md` 明确要求
//! 调试器信号**只作遥测**、不得在设备操作中途退出进程。本模块**复用**该
//! 探针作为信号源之一，不另起一套 FFI，避免两套结论打架。
//!
//! 额外的平台原语（Windows `IsDebuggerPresent`）作为**独立的第二信号**，
//! 覆盖未加壳 / 开发态等 VMP 探针不可用的场景。

use nwflash_protection::{IntegrityProbe, IntegrityTelemetry};

/// 调试器检测结果。`Attached` 只表示"观察到调试痕迹"，不代表镜像不可信。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebuggerStatus {
    /// 未观察到调试痕迹。
    Absent,
    /// 观察到调试器：VMP 遥测与平台原语中至少一个为真。
    Attached,
}

/// 操作所处的阶段。决定检测到调试器时的处置方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationPhase {
    /// 尚未开始写入设备（预检、登录、选择镜像等）。
    BeforeWrite,
    /// 正在写入设备。此时**任何**中断都可能导致变砖。
    DuringWrite,
}

/// 检测到调试器时的处置决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntiDebugDecision {
    /// 放行。
    Proceed,
    /// 写入前发现调试器：拒绝服务。
    RefuseService,
    /// 写入中发现调试器：挂起等待用户处置，**不中断设备会话**。
    SuspendAndWarn,
}

/// 平台原语：Windows 用 `IsDebuggerPresent`；其他平台返回 `false`（未知即不阻断）。
///
/// Linux 的 `ptrace(PTRACE_TRACEME)` 在本项目不适用：那是**被调试方主动
/// 请求**成为被跟踪者的语义，用于自我防护时需要自己再 detach，且在 Tauri
/// 桌面端（本项目只发布 Windows）没有实际调用点。这里保留平台分支，
/// 但不虚构一个永远不会被验证的非 Windows 实现。
#[cfg(windows)]
pub fn platform_debugger_present() -> bool {
    use windows_sys::Win32::System::Diagnostics::Debug::IsDebuggerPresent;
    // SAFETY: `IsDebuggerPresent` 无参数、无前置条件，只读取当前进程的
    // PEB 标志位，不涉及任何调用方提供的指针或生命周期。
    unsafe { IsDebuggerPresent() != 0 }
}

#[cfg(not(windows))]
pub fn platform_debugger_present() -> bool {
    false
}

/// 综合平台原语与 VMP 遥测，判定当前是否观察到调试器。
pub fn is_debugger_attached(probe: &dyn IntegrityProbe) -> DebuggerStatus {
    if platform_debugger_present() {
        return DebuggerStatus::Attached;
    }
    match nwflash_protection::verify_image_integrity(probe).telemetry {
        IntegrityTelemetry::DebuggerPresent
        | IntegrityTelemetry::DebuggerAndVirtualMachinePresent => DebuggerStatus::Attached,
        IntegrityTelemetry::None | IntegrityTelemetry::VirtualMachinePresent => {
            DebuggerStatus::Absent
        }
    }
}

/// 两段式处置状态机。
///
/// 语义：
/// - `BeforeWrite` + 有调试器 → [`AntiDebugDecision::RefuseService`]
/// - `DuringWrite` + 有调试器 → [`AntiDebugDecision::SuspendAndWarn`]
///   （**绝不**返回任何"退出/中止"类决定）
/// - 无调试器 → [`AntiDebugDecision::Proceed`]
///
/// 这个函数是纯函数：它只做决策，不做副作用。真正的挂起由调用方的
/// `tokio` 任务实现，退出由既有的 exit supervisor 实现。
pub fn decide(status: DebuggerStatus, phase: OperationPhase) -> AntiDebugDecision {
    match (status, phase) {
        (DebuggerStatus::Absent, _) => AntiDebugDecision::Proceed,
        (DebuggerStatus::Attached, OperationPhase::BeforeWrite) => {
            AntiDebugDecision::RefuseService
        }
        (DebuggerStatus::Attached, OperationPhase::DuringWrite) => {
            AntiDebugDecision::SuspendAndWarn
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nwflash_protection::IntegritySignals;

    struct StubProbe(IntegritySignals);

    impl IntegrityProbe for StubProbe {
        fn signals(&self) -> IntegritySignals {
            self.0
        }
    }

    #[test]
    fn absent_debugger_proceeds_in_both_phases() {
        assert_eq!(
            decide(DebuggerStatus::Absent, OperationPhase::BeforeWrite),
            AntiDebugDecision::Proceed
        );
        assert_eq!(
            decide(DebuggerStatus::Absent, OperationPhase::DuringWrite),
            AntiDebugDecision::Proceed
        );
    }

    #[test]
    fn debugger_before_write_refuses_service() {
        assert_eq!(
            decide(DebuggerStatus::Attached, OperationPhase::BeforeWrite),
            AntiDebugDecision::RefuseService
        );
    }

    /// 本模块最重要的一条不变量：写入中绝不产生任何退出/中止类决定。
    #[test]
    fn debugger_during_write_suspends_and_never_exits() {
        assert_eq!(
            decide(DebuggerStatus::Attached, OperationPhase::DuringWrite),
            AntiDebugDecision::SuspendAndWarn
        );
    }

    #[test]
    fn every_decision_is_one_of_three_closed_variants() {
        // 穷举两个维度，证明决定集合是闭的、不存在"退出"这一类。
        let statuses = [DebuggerStatus::Absent, DebuggerStatus::Attached];
        let phases = [OperationPhase::BeforeWrite, OperationPhase::DuringWrite];
        for status in statuses {
            for phase in phases {
                let decision = decide(status, phase);
                assert!(matches!(
                    decision,
                    AntiDebugDecision::Proceed
                        | AntiDebugDecision::RefuseService
                        | AntiDebugDecision::SuspendAndWarn
                ));
            }
        }
    }

    #[test]
    fn vmp_telemetry_debugger_signal_is_detected() {
        let probe = StubProbe(IntegritySignals::available(true, true, true, false));
        assert_eq!(is_debugger_attached(&probe), DebuggerStatus::Attached);
    }

    #[test]
    fn vmp_telemetry_without_debugger_is_absent() {
        let probe = StubProbe(IntegritySignals::available(true, true, false, false));
        assert_eq!(is_debugger_attached(&probe), DebuggerStatus::Absent);
    }

    /// 虚拟机存在**不算**调试器：`README` 要求 VM 检测只作遥测，
    /// 且本项目大量用户跑在虚拟化环境里，误判会直接拒绝正常刷机。
    #[test]
    fn virtual_machine_alone_is_not_treated_as_a_debugger() {
        let probe = StubProbe(IntegritySignals::available(true, true, false, true));
        assert_eq!(is_debugger_attached(&probe), DebuggerStatus::Absent);
    }
}