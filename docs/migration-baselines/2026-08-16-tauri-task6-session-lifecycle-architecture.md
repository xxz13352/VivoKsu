# 2026-08-16 NWflash Tauri 迁移 Task6 会话生命周期架构补齐（5.3codex）

## 架构目标

- 保证 `SessionLifecycle` 的 `force_exit` 与 `update_required` 信号不会仅停留在日志层，而是进入前端可感知闭环。
- 在服务端回调路径中保持“**先取消当前操作，再安全退出/提示**”的顺序，避免半执行 flash 任务被异步打断后残留。
- 前端仅在 UI 层消费事件，不在 Rust 内嵌入页面文案与展示策略。

## 边界定义

- 事件来源：`nwflash-infrastructure::api_client` 的 `HeartbeatResult` 与 `UpdateRequiredInfo`。
- 转发路径：`SessionLifecycle` 回调 -> `AppState` 内部 `session_events` -> Tauri 事件总线。
- 事件分发：
  - `session:force-exit`
    - payload: `{ "reason": "..." }`
    - Tauri 行为：强制取消 `OperationCoordinator`、等待空闲窗口、派发会话事件、请求进程退出。
  - `session:update-required`
    - payload: `{ "message", "latest", "minVersion", "downloadUrl" }`
    - Tauri 行为：先取消当前操作，再派发更新事件，不直接退出进程。

## 本次实现文件

- `src-tauri/crates/nwflash-tauri/src/lib.rs`
  - 新增 `SessionLifecycleEvent` 与 `session_events` 通道；
  - `AppState::new()` 绑定 `SessionLifecycle` 回调到事件通道；
  - 增加 `bind_session_events()`，消费事件并触发 `session:*` 事件与 `OperationCoordinator` 停止逻辑。
- `src-tauri/crates/nwflash-tauri/Cargo.toml`
  - 增加 `tokio` sync/time 依赖用于会话事件监听与阻塞等待。
- `src/app/ipc-events.ts`
  - 增加前端事件契约：`session:force-exit`、`session:update-required`。
- `src/app/App.tsx`
  - 注册会话事件监听；
  - 接收强退/更新信号时清空顶部操作态并切换登录态，更新提示文案。
- `src/AppSessionLifecycle.test.tsx`
  - 新增前端测试：会话强退与强制更新事件都触发未登录态与提示展示。

## 验证命令（本轮最小）

- `cargo test -p nwflash-application --test session_lifecycle`
- `cargo test -p nwflash-tauri --lib`
- `npm --prefix src/Nwflash.Desktop run test -- AppSessionLifecycle.test.tsx`
