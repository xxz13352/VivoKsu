# 2026-08-16 Tauri 迁移 Task7 使用日志与操作日志持久化架构说明（5.3codex）

## 1. 任务边界（本轮）

- 从 WPF `UsageLogUploader` 与 `OperationLogService` 迁移使用日志闭环到 Tauri。
- 在不改变前端行为的前提下，完成：
  - 使用日志（Usage Log）批量上传队列；
  - 操作日志（Operation Log）持久化与快照读取；
  - session 生命周期事件下（会话启停、强退、更新）触发 flush/清理动作。
- 不改动 API 契约、云端接口、设备刷写流程。

## 2. 对齐基线

- `Task6` 会话生命周期闭环（`session:force-exit` / `session:update-required`）。
- WPF 参考：`UsageLogUploader.cs`、`OperationLogService.cs`、`AppComposition.cs`。
- Rust 参考：`nwflash-application` 操作器模型 + `nwflash-infrastructure` API Client + `nwflash-tauri` 命令层。

## 3. 本轮核心实现（文件级）

- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/usage_reporter.rs`
  - 新建 `UsageLogReporter`（Rust 侧 usage 日志上传器）：
    - `FLUSH_INTERVAL = 30s`；
    - `FLUSH_THRESHOLD = 20`；
    - `MAX_BATCH_SIZE = 100`；
    - `flush_gate` 串行化上传；
    - 无 token 时不丢队列，失败批次回退到缓冲重试。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
  - `AppState` 新增：
    - `session_token: Arc<RwLock<Option<String>>>`（共享给 session 与 usage 上报）；
    - `usage_reporter: Arc<UsageLogReporter>`；
    - `operation_log_store: Arc<OperationLogStore>`；
  - `OperationCoordinator` 依赖注入：
    - `Some(usage_reporter)` 作为 `UsageReporter`；
    - `Some(operation_log_buffer)` 作为 `OperationLogger`。
  - `bind_session_events` 保持“取消操作后再退出/弹窗”的顺序，沿用既有会话事件总线。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/session.rs`
  - `session_start` 启动成功后触发 `usage_reporter.start_if_needed()`；
  - `session_stop` 结束前执行一次 `usage_reporter.flush().await`。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/auth.rs`
  - `auth_logout` 先异步触发 `usage_reporter.flush()`，再清空 token。
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/operation_log.rs`
  - `operation_logs_snapshot` 改为返回 `operation_log_store.snapshot()`，避免只读内存缓存。

## 4. 关键行为映射（WPF → Rust）

- WPF `UsageLogUploader`
  - 阈值达到阈值立即触发 flush：Rust 保留阈值触发逻辑；
  - 周期 flush：Rust 使用 `tokio` interval 定时 30s；
  - 批量上传 + chunk：Rust 按 `MAX_BATCH_SIZE=100` 切片；
  - 上传失败回退：Rust 将失败 chunk 重新 push_front 到 pending，保留重试。
- WPF `OperationLogService`
  - 最近 500 条 + 本地文件持久化 + 2MB 轮转：Rust 使用 `OperationLogStore` 与 `with_default_path(500)`，并保留 JSON 行存储与滚动。
- 会话闭环
  - WPF 登录后首次会话启动时开启 usage reporter；Rust 在 `session_start` 后调用 `start_if_needed`；
  - WPF 退出会话先尝试 flush：Rust 在 `session_stop` 与 `auth_logout` 均触发 flush 兜底。

## 5. 5.3codex 当前进度

- 核心迁移链路已打通：`OperationCoordinator → UsageLogReporter → CloudflareClient.upload_usage_logs`。
- 运行日志链路已改为 `OperationLogStore` 持久化源，Tauri 命令返回真实 snapshot。
- 经过最小验证：会话生命周期测试与 tauri lib 测试均通过。

## 6. 最小化验证命令

- `cargo test -p nwflash-application --test session_lifecycle`
- `cargo test -p nwflash-application --test operation_coordinator`
- `cargo test -p nwflash-tauri --lib`
