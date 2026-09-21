//! 写入中途的反调试挂起守卫。
//!
//! ## 为什么必须挂起而不是中止
//!
//! 用户在写入分区时，fastboot 会话正处于"已发送部分数据"的状态。此时若
//! 因为检测到调试器就 `Err` 返回或退出进程，设备会停在**写了一半**的
//! 分区上——这是变砖的直接原因。
//!
//! 因此本模块只提供**挂起**语义：检测到调试器时，把写入循环挡在
//! `wait_until_cleared()` 上等待，期间：
//! - 不发新命令、不改设备状态；
//! - 不 drop 任何凭据、不关闭会话；
//! - 由 UI 提示用户处置，用户确认后才继续。
//!
//! ## 与取消的关系
//!
//! **挂起不是取消**。取消是用户主动中止（设备会被留在已知状态，流程走
//! 收尾逻辑）；挂起是"暂停并等待"，流程不推进也不收尾。二者在类型上
//! 就是两个不同的东西，调用方必须分别处理。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// 写入中途的挂起闸门。
///
/// 由命令层创建、UI 层解锁（用户点击"我已处理"），写入循环在每个分区
/// 边界调用 [`SuspendGate::wait_until_cleared`]。
#[derive(Debug, Clone, Default)]
pub struct SuspendGate {
    suspended: Arc<AtomicBool>,
}

impl SuspendGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 挂起闸门。幂等：重复调用不会叠加状态。
    pub fn suspend(&self) {
        self.suspended.store(true, Ordering::Release);
    }

    /// 解除挂起。只能由用户处置后调用。
    pub fn clear(&self) {
        self.suspended.store(false, Ordering::Release);
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::Acquire)
    }

    /// 挂起状态下返回 `true`，表示"现在不能继续写设备"。
    ///
    /// 纯查询，不做等待——等待由异步调用方实现（见 `wait_until_cleared`），
    /// 这样本类型不依赖任何运行时，可以被同步的写入循环直接使用。
    pub fn blocks_progress(&self) -> bool {
        self.is_suspended()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_starts_unsuspended() {
        let gate = SuspendGate::new();
        assert!(!gate.is_suspended());
        assert!(!gate.blocks_progress());
    }

    #[test]
    fn suspend_blocks_progress_until_cleared() {
        let gate = SuspendGate::new();
        gate.suspend();
        assert!(gate.blocks_progress());
        gate.clear();
        assert!(!gate.blocks_progress());
    }

    #[test]
    fn suspend_is_idempotent() {
        let gate = SuspendGate::new();
        gate.suspend();
        gate.suspend();
        gate.clear();
        // 单次 clear 就应完全解除：不存在"挂起计数"残留。
        assert!(!gate.blocks_progress());
    }

    #[test]
    fn clones_share_one_gate() {
        let gate = SuspendGate::new();
        let observer = gate.clone();
        gate.suspend();
        // UI 层持有的克隆必须能看到后台的挂起状态。
        assert!(observer.is_suspended());
        observer.clear();
        assert!(!gate.is_suspended());
    }
}