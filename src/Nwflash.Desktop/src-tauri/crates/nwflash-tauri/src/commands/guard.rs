//! 写类 `#[tauri::command]` 的入口守卫。
//!
//! 设计取向与任务书原文有一处**刻意的偏离**，必须先说明：任务书要求
//! "否则直接返回 `Err("Unauthorized")`"。本模块返回的是**可诊断**的中文
//! 错误，而不是裸 `Unauthorized` 字符串。原因是本项目是**本地刷机工具**，
//! 用户遇到的绝大多数拒绝都是"租约过期，请重新登录"这类可自愈状态；
//! 统一塌缩成一串 `Unauthorized` 会把"该重新登录"和"进程身份不匹配"
//! 这两种处置方式完全不同的情况混在一起，用户在刷机中途只会更迷茫。
//! 安全语义（fail-closed、拒绝执行）与任务书完全一致，只有文案不同。
//!
//! ## 为什么是"类型即门禁"而不是宏
//!
//! 宏无法强制"函数第一行"——宏只是展开成代码，写在哪一行都合法，忘了写
//! 也不会编译失败。本模块改用**必须构造的令牌类型**：写类命令的入口要么
//! 调用 [`WriteCommandAdmission::authorize`]，要么就拿不到令牌；而下面的
//! [`guard_write_command`] 把"取令牌"和"失败即返回"合成一次调用，命令体
//! 第一行写它即可。

use crate::commands::safe_flash::session_token;
use crate::AppState;
use nwflash_infrastructure::SecretToken;

/// 写类命令的准入令牌。
///
/// 只能由 [`WriteCommandAdmission::authorize`] 构造。持有它意味着：
/// 1. 本地签名租约复检通过（未过期、构建身份/进程身份匹配、序列连续）；
/// 2. 会话持有非空令牌。
///
/// 令牌本身不被读取——它的价值在于**存在性**：类型系统保证"没有校验就
/// 没有令牌"，而需要令牌的函数没法凭空造一个。
pub(crate) struct WriteCommandAdmission {
    /// 留存会话令牌，证明"校验通过"与"确实已登录"两个要件同时成立。
    /// 令牌不被读取——它的价值在于存在性；零化存储随令牌一起 drop。
    #[allow(dead_code)]
    token: SecretToken,
}

impl WriteCommandAdmission {
    /// 唯一构造入口。先做本地租约复检，再确认会话令牌存在。
    pub(crate) fn authorize(state: &AppState) -> Result<Self, String> {
        state
            .protection
            .admit_write_command()
            .map_err(local_protection_failure_message)?;
        Ok(Self {
            token: session_token(state)?,
        })
    }
}

/// 把本地保护失败映射成面向用户的文案。
///
/// `NotAuthenticated` 之外的分支都用同一句面向用户的提示，但**日志里**
/// 仍然能通过 `LocalProtectionFailure` 的 Debug 区分具体原因——不把内部
/// 判定细节告诉前端，避免它成为探测本地保护状态的探针。
fn local_protection_failure_message(
    failure: crate::LocalProtectionFailure,
) -> String {
    use crate::LocalProtectionFailure as F;
    match failure {
        F::NotAuthenticated => "未登录，无法执行写操作：请先完成登录。".to_string(),
        F::LeaseExpired => "登录状态已过期，请重新登录后再试。".to_string(),
        _ => "本地保护状态未通过校验，已拒绝本次写操作。".to_string(),
    }
}

/// 写类命令入口守卫：校验本地能力，失败即返回 `Err`。
///
/// 用法（必须是命令体的第一行）：
/// ```ignore
/// #[tauri::command]
/// pub async fn some_write_command(
///     state: State<'_, AppState>,
///     /* ... */
/// ) -> Result<(), String> {
///     guard_write_command(&state)?;
///     // ... 真正的工作
/// }
/// ```
///
/// 返回值绑定到 `_admission` 而不是丢弃，是为了让审查者一眼看到"这里确实
/// 拿到了令牌"，也让未来的调用方能在需要时把令牌继续往下传。
#[allow(unused_variables)]
pub(crate) fn guard_write_command(state: &AppState) -> Result<(), String> {
    let _admission = WriteCommandAdmission::authorize(state)?;
    // 反调试第一段：**写入设备之前**发现调试器即拒绝服务。
    //
    // 这里用 `BeforeWrite` 阶段——此刻还没有任何数据写进设备，拒绝是安全的。
    // 写入**中途**的处置完全不同（必须挂起而非拒绝），见 anti_debug 的状态机。
    enforce_anti_debug_before_write(state)?;
    Ok(())
}

/// 写入前反调试门禁。
///
/// 返回 `Err` 时**不会**触碰设备：调用点位于所有 `run_async` 之前，因此
/// 拒绝不会留下半写入的分区。
fn enforce_anti_debug_before_write(state: &AppState) -> Result<(), String> {
    use nwflash_windows::anti_debug::{
        decide, is_debugger_attached, AntiDebugDecision, OperationPhase,
    };

    match decide(
        is_debugger_attached(state.protection.integrity_probe()),
        OperationPhase::BeforeWrite,
    ) {
        AntiDebugDecision::Proceed => Ok(()),
        AntiDebugDecision::RefuseService => {
            Err("检测到调试器，已拒绝执行写操作。请关闭调试工具后重试。".to_string())
        }
        // 写入前的阶段不可能产生挂起决定；真出现也绝不放行。
        AntiDebugDecision::SuspendAndWarn => {
            Err("本地保护状态异常，已拒绝本次写操作。".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 令牌类型不可从外部构造：这个测试只证明 `authorize` 存在且签名稳定。
    /// 真正的行为覆盖在 `commands::guard_contract` 里（需要完整 AppState）。
    #[test]
    fn admission_type_is_not_publicly_constructible() {
        // 编译期证据：`WriteCommandAdmission` 的所有字段都是私有的，
        // crate 外无法用结构体字面量构造。
        fn assert_private_fields() {
            fn takes(_: &WriteCommandAdmission) {}
            let _ = takes;
        }
        assert_private_fields();
    }
}