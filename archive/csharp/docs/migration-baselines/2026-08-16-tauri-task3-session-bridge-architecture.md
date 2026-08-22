# 2026-08-16 Tauri 迁移 Task3 架构说明（会话门禁与会话生命周期桥接）

## 1. 目标与边界

- 目标：把 WPF 的会话与操作控制语义（`OperationCoordinator + HeartbeatService`）迁移到 Rust/Tauri，先保证行为等价，不改前端流程与 cloudflare。
- 范围：本文件覆盖 `Task 3`（会话门禁 + 在线会话生命周期）。不包含刷写/传输核心实现，不包含 API 合约层业务实现（Task2 另行管理）。
- 外部约束：
  - 不修改 `cloudflare/**`。
  - token 不落盘，优先保持进程内会话态。
  - 采用“先行为等价、再接页面”的顺序，保留 WPF 的禁用态与错误映射。

## 2. 对齐基线与输入

- 基线约束：
  - [docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md](docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md)
  - [docs/migration-baselines/api-contract-cases.md](docs/migration-baselines/api-contract-cases.md)
  - [docs/superpowers/plans/2026-08-16-tauri-rust-migration-kickoff.md](docs/superpowers/plans/2026-08-16-tauri-rust-migration-kickoff.md)
- WPF 参考源码：
  - `src/VivoKsu.App/Services/OperationCoordinator.cs`
  - `src/VivoKsu.App/Services/HeartbeatService.cs`
  - `src/VivoKsu.App/Services/AppComposition.cs`

## 3. 5.3codex 现状与迁移切口

### 3.1 已实现 crate 结构

- `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/operation_coordinator.rs`
  - `OperationCoordinator`、`OperationState`、`OperationProgress`、`OperationSnapshot` 等核心类型。
  - 统一职责：单点操作门禁、任务快照发布、取消控制、日志与 usage 上报入口、进度节流。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/session_lifecycle.rs`（新增）
  - `SessionLifecycle`、`SessionState`、`SessionError`。
  - 统一职责：会话启动/停止、心跳定时器、goodbye、强制退出与更新事件回调。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/lib.rs`
  - 导出 `session_lifecycle` 模块并统一类型边界。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/session.rs`（新增）
  - `session_start` / `session_stop` / `session_state` Tauri 命令。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
  - `AppState` 新增 `session_lifecycle`，并在启动时注入回调：
    - `on_force_exit`（强制退出）
    - `on_update_required`（更新弹窗路径）
  - 注册 session 相关命令到 invoke handler。

## 4. 架构图（Rust 侧）

```text
          +------------------------------+
          |      Tauri AppState          |
          |------------------------------|
          | session_lifecycle:           |
          |   SessionLifecycle           |
          | operation_coordinator:       |
          |   OperationCoordinator       |
          | api_client: Arc<CloudflareClient> |
          +------------------------------+
                       |
                       | owns shared infra client + callbacks
            +----------+------------------+
            |                             |
            v                             v
  +--------------------+         +----------------------+
  | session_lifecycle  |         | operation_coordinator |
  | - start/stop/goodbye|         | - run()/authorize()   |
  | - heartbeat loop    |         | - state snapshots     |
  | - force_exit/update |         | - usage buffering     |
  +--------------------+         +----------------------+
            |                             |
            | invoke events                | state/tick
            v                             v
  +--------------------+         +----------------------+
  | front-end bridge    |         | log + API 上传接口      |
  | session_* commands  |         | (CloudflareClient)    |
  +--------------------+         +----------------------+
```

## 5. 关键行为映射（WPF → Rust）

### 5.1 操作门禁与并发

- WPF：
  - `RunAsync` 使用 `SemaphoreSlim` + `CancellationTokenSource`，并发时直接拒绝，不排队。
- Rust（本次实现）：
  - `OperationCoordinator` 保留 `Semaphore` + `InProgress` guard。
  - `run` 在已有执行任务时立即返回 `Err("已有任务正在进行中，请等待其完成或先取消。")`。
- 验收点：
  - 并发请求不会导致任务排队；后续调用必须看到拒绝文本并立刻返回。

### 5.2 取消传播与退出顺序

- WPF：
  - 每个操作都可取消；退出时先取消再善后。
- Rust：
  - `OperationCoordinator::run` 维护 `current_cancel`；
  - 任务通过 `tokio::select!` 监听 `Abortable` / `Cancel`；
  - `complete` 路径记录状态（成功/取消/失败）并上报 usage。
- `SessionLifecycle::stop`：
  - 先 `goodbye` 再取消并清理当前任务；
  - 清理时会等待进行中任务结束（`JoinHandle` join / 空间清理）；
  - 再广播 `SessionState`。

### 5.3 进度更新节流与优先级

- WPF：
  - 阶段变化可即时更新，纯进度变化 100ms 节流，减少 UI 负载。
- Rust：
  - `OperationCoordinator` 节流写快照至 `last_progress_report`；
  - 仅在阶段变更或经过 100ms 时进行快照。
- 说明：节流粒度与 WPF 行为一致，不改统一进度优先级策略。

### 5.4 心跳与生命周期

- WPF：心跳间隔 5s、请求超时 10s、goodbye 超时 3s；收到 `force_exit`/401/403 触发下线；426 强更。
- Rust：
  - `session_lifecycle.rs` 常量：
    - `HEARTBEAT_INTERVAL = 5s`
    - `HEARTBEAT_REQUEST_TIMEOUT = 10s`
    - `GOODBYE_TIMEOUT = 3s`
  - `start(session_id, token)` 建立 loop；
  - `heartbeat(session_id, active, client_version)` 调用 `CloudflareClient::heartbeat`；
  - 返回 `UpdateRequired` / `HeartbeatForceExit` / `HeartbeatAuthFailed` 统一映射回调或状态机。

### 5.5 会话 token 与会话态

- token 为空时 `session_start` 直接失败（与 WPF 门禁一致）；
- `session_state` 可返回 `running/healthy/session_id/has_token`；
- `is_running` 与 `is_healthy` 由 `SessionState` 原子状态管理，主进程可据此切换前端登出/禁用态。

## 6. 错误与回调语义

- `SessionError` 枚举覆盖：
  - `NoToken`
  - `AlreadyRunning`
  - `AlreadyStopped`
  - `Heartbeat`
  - `Operation`
  - `UpdateRequired`
  - `SessionNotStarted`
  - `InvalidSessionId`
- 回调执行：
  - `on_force_exit(reason)` / `on_update_required(...)` 由 `SessionLifecycle` 调用；
  - 统一包裹 `catch_unwind`，避免回调 panic 传播导致任务退出。

## 7. 测试锚点与验证方式

### 7.1 已有覆盖（Task3）

- `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/operation_coordinator.rs`
  - 并发拒绝
  - 取消传播
  - usage 上报
  - 进度节流
  - state 快照更新
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/session_lifecycle.rs`
  - 启停与 goodbye
  - 强制退出回调
  - 更新要求回调
  - 会话 id/token 边界校验

### 7.2 最小化执行命令（仅本任务）

- `cargo test -p nwflash-application --test operation_coordinator`
- `cargo test -p nwflash-application --test session_lifecycle`

### 7.3 Tauri 编译门禁

- `cargo test -p nwflash-tauri --lib --no-run`
  - 确认新 session 命令与 `AppState` 组装不影响链接。
- `npm --prefix src/Nwflash.Desktop test -- App AppShell window-state`
  - 覆盖前端状态渲染、导航与进度联动组件的回归。

## 8. 与前端行为联动计划

- `session_*` 命令仅提供控制与状态查询能力；
- 本阶段已完成：`AppShell/window-state` 订阅 `operation` 事件，基于 `operation:state`/`operation:progress` 驱动右上角进度文案与登出禁用态。
  - `CanLogout = !isBusy && !isDeviceBusy`
  - `ProgressText = session / operation snapshot`

### 8.1 已落地事件对齐

- `nwflash-tauri` 侧事件：
  - `operation:state`: 当前是否 busy 与 busyKinds；
  - `operation:progress`: 单项进度文本、percent 与阶段名；
  - 事件由 `AppState` 在 `setup` 时通过 `OperationCoordinator::subscribe_state` 订阅并异步 emit。
- `src/Nwflash.Desktop/src/app/App.tsx`:
  - 运行时监听上述事件并更新 `operations` 状态；
  - 通过 `session_state` 回写登录态，避免脱钩。
- 说明：当前 busy-kind 规则采用 operation title 关键词映射（`lineFlash/quick/safeFlash/firmwareExtract/device`），用于在没有完整页面级 source tag 时保持可用输出；后续刷写功能上马时可替换为显式 kind tag。

## 9. 下一步（Task4 依赖）

1. `session_state` 作为统一状态源订阅到前端，连接按钮禁用、在线进度展示。
2. `SessionLifecycle` 与 device 监视事件桥接（在线刷新/force_exit）进入页面状态机（在 Task4 后段实现）。
3. 本轮先补传输接口层：`nwflash-windows` 命令构建、`nwflash-application` quick flash/file_transfer 编排最小入口（详见 `2026-08-16-tauri-task4-transfer-interface-architecture.md`）。
4. 用真实端到端场景（刷写进行中 → heartbeat 强退）验证“先退出动作，再空闲退出窗口”不打断刷写。

## 10. 风险与回滚线

- `SessionLifecycle` 当前采用 `watch` + `join` 的两层终止控制；复杂时序下可能出现临界窗口。若出现竞态：
  - 回退策略：先检查 `session token`, `session id` 生命周期闭环，再细化 `CancellationToken` 传播。
- API 回调是 `CloudflareClient::heartbeat` 的返回类型契约；一旦心跳契约变更应先回 Task2 更新，再联调 Task3 语义。
