# 2026-08-16 Tauri 迁移 Task4 传输链路最小接口面

## 1. 任务边界（本轮）

- 本任务为 Task4（刷写/传输/读取链路）最小接口面，不接入 UI 命令。
- 目标只包含：
  - `nwflash-windows` 侧命令构建能力最小集；
  - `nwflash-application` 侧 quick flash 与 file transfer 编排入口；
  - 参数边界与边界条件回归测试；
  - 不引入真实设备刷写执行逻辑。

## 2. 目标代码变更

- `crates/nwflash-windows/src/process.rs`
  - `ProcessCommand` 升级为拥有型参数对象（`args`、`working_directory`、`environment`）；
  - 新增 `run_command_with_timeout`，用于命令执行超时与进程终止；
  - 增加参数边界校验和环境变量透传；
  - 保留数组参数执行（无字符串拼接）。
- `crates/nwflash-windows/src/platform_tools.rs`
  - 新增 `PlatformTools`：维护 adb / fastboot 可执行路径；
  - 统一串口 + 参数构建入口；
  - 提供 `ADB` 环境变量注入向量。
- `crates/nwflash-windows/src/device_transport.rs`
  - 新增 `DeviceTransport`，实现 fastboot flash/erase、ADB-root pull/push/erase 的命令参数构建；
  - 添加串口与路径边界校验（设备路径格式、空值）。
- `crates/nwflash-application/src/quick_flash.rs`
  - 新增 `QuickFlashService`，基于 `PartitionExecutionPlan` 构建命令序列；
  - 支持 Write / Erase 与通道分流（Fastboot/AdbRoot）。
- `crates/nwflash-application/src/file_transfer.rs`
  - 新增 `FileTransferService`，构建 pull/push 命令。
- `crates/nwflash-application/src/lib.rs`
  - 导出 `QuickFlashService` 与 `FileTransferService`。
- `crates/nwflash-application/Cargo.toml`
  - 增加 `nwflash-windows` 依赖。
- `crates/nwflash-tauri/src/commands/{quick_flash.rs,file_transfer.rs}`
  - 新增命令层单元测试，覆盖命令 DTO 输出与错误回传映射。

## 3. 测试覆盖（本轮）

- `crates/nwflash-windows/src/process.rs`
  - 串口/路径边界 + ADB 环境变量 + 长耗时命令终止。
- `crates/nwflash-windows/src/platform_tools.rs`
  - 串口为空校验、ADB 环境变量构造。
- `crates/nwflash-windows/src/device_transport.rs`
  - 串口为空与设备路径边界、fastboot erase 命令形态。
- `crates/nwflash-application/tests/quick_flash.rs`
  - fastboot flash 命令形态、空串口、未解析通道拒绝。
- `crates/nwflash-application/tests/file_transfer.rs`
  - pull 命令形态、坏设备路径、空串口。

## 4. 最小化测试命令

- `cargo test -p nwflash-windows`
- `cargo test -p nwflash-application --test quick_flash --test file_transfer`

## 5. 下一步

- 任务4 Step2/Step3：接入 `nwflash-tauri` 命令层（`quick_flash`、`file_transfer`）；
- 统一事件 contract 到前端（刷写/传输阶段进度）；
- 逐步引入 UI 页面对接（先最小页面）。

## 6. 5.3codex 当前进度（2026-08-16）

- 已完成：Task4 Step3（`nwflash-tauri` 命令层接入）可编译通过。
- 关键修正：`QuickFlashService` 与 `FileTransferService` 现返回应用层 `CommandSpec`，`nwflash-tauri` 仅消费应用 DTO，不直接依赖 `nwflash-windows`。
- 本轮最小验证：
- `cargo test -p nwflash-application --test quick_flash --test file_transfer`
- `cargo test -p nwflash-tauri --lib --no-run`
- `cargo test -p nwflash-tauri --lib`
- 下一步：补齐前端最小事件快照断言（命令预检路径）后再推进 Task4 Step4。
- 阶段进展：已完成前端命令预检最小闭环（`QuickFlashPage` 与 `FileManagerPage`）。
- 阶段进展：新增操作日志快照通路：
  - `AppState` 新增共享 `OperationLogBuffer`，注入 `OperationCoordinator` 的 `OperationLogger`。
  - 新增 Tauri 命令 `operation_logs_snapshot`，返回 `Vec<OperationLogEntry>`。
  - 前端 `OperationLogPage` 与测试落地（mock invoke、成功两条日志、空日志显示）。
- 最近验证：
  - `npm --prefix src/Nwflash.Desktop run test -- QuickFlashPage.test.tsx FileManagerPage.test.tsx`
  - `cargo test -p nwflash-tauri --lib`
  - `npm --prefix src/Nwflash.Desktop run test -- OperationLogPage.test.tsx`
  - `cargo test -p nwflash-application --test quick_flash --test file_transfer`
- 进度事件对齐补充：
  - Rust `bind_operation_events` 改为仅推送 `operation:snapshot`，包含完整 `OperationStateSnapshot`（kind/title/stage/progress 等）并保留内部 `isBusy` 标记；
  - 前端 `App.tsx` 监听 `operation:snapshot`，按快照重建 `operations` 渲染，移除 `operation:state` 与 `operation:progress` 双事件。
  - 依赖文档同步到 `docs/superpowers/plans/2026-08-15-tauri-rust-migration.md` Task6 要求。
- 运维稳定性补充：
  - `src-tauri/crates/nwflash-tauri/src/main.rs` 安装 panic hook，按 `%LOCALAPPDATA%\\Nwflash\\crash.log` 追加记录异常，进程异常退出前保留 trace。
- 最近验证（含进度事件切换）：
  - `npm --prefix src/Nwflash.Desktop run test -- AppShell window-state`
  - `npm --prefix src/Nwflash.Desktop run test -- QuickFlashPage.test.tsx FileManagerPage.test.tsx OperationLogPage.test.tsx`
  - `cargo test -p nwflash-tauri --lib`
- 2026-08-16 追加进展（执行路径最小闭环）
  - 为 `nwflash-tauri` 新增执行命令：
    - `quick_flash_execute_commands`
    - `file_transfer_run_pull_command`
    - `file_transfer_run_push_command`
  - 三个执行命令通过 `OperationCoordinator` 发起运行、上报 `operation:snapshot`，并按退出码映射到 `ExternalTool` 错误；
  - 前端 `QuickFlashPage` 与 `FileManagerPage` 补充“执行”按钮和执行返回体预览，保留“命令预检”能力；
  - 最新最小验证命令：
- `npm --prefix src/Nwflash.Desktop run test -- QuickFlashPage.test.tsx FileManagerPage.test.tsx`
- `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --lib`

## 2026-08-16 追加进展（可取消执行关闭进程树）

- 任务4 Step3.5：补齐执行路径中“可取消即中止进程树”的语义。
- 变更：
  - `crates/nwflash-windows/src/process.rs`
    - 新增 `run_command_with_cancel(spec, timeout, should_cancel)`；
    - `run_command` / `run_command_with_timeout` 委托到 `run_command_with_cancel`；
    - 增加 `terminate_process_tree`：Windows 使用 `taskkill /F /T /PID` 终止子树；
    - 新增测试 `run_command_with_cancel_stops_process_when_cancelled`；
  - `crates/nwflash-tauri/src/commands/quick_flash.rs`
    - `quick_flash_execute_commands` 执行时改为 `run_command_with_cancel(..., cancellation.is_cancelled)`；
  - `crates/nwflash-tauri/src/commands/file_transfer.rs`
    - `file_transfer_run_pull_command` 与 `file_transfer_run_push_command` 执行时改为 `run_command_with_cancel(..., cancellation.is_cancelled)`。
- 下一步验证：
  - `cargo test -p nwflash-windows --lib`
  - `cargo test -p nwflash-tauri --lib`
  - `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test build_smoke`
