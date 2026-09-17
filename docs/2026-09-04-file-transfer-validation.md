# 文件传输全链路验证报告

> 历史基线说明：本文记录 P1 修复前的验证现场，其中 `0xc0000139`、取消入口和直接写最终目标等缺口后来分别由 `e5102cc`、`82cc192`、`8f759ef` 处理。当前修复后证据见 [P1 后验证报告](2026-09-04-file-transfer-post-p1-validation.md)，剩余发布前工作见 [P1-03 计划](2026-09-04-file-transfer-p1-03-plan.md)。以下原始结果保留用于追溯，不应当作当前状态。

**任务**：阶段 B（文件传输逐步验证）  
**审计角色**：验证与代码接力审计员  
**日期**：2026-09-04（Asia/Shanghai）  
**范围**：文件选择 → React 状态 → Tauri IPC → 登录/会话/coordinator → ADB 参数 → 进程输出/退出码 → 进度/取消 → partial 清理 → 日志/重试。  
**明确不在本轮目标内**：哈希校验、跨步骤 serial 绑定、真机连接。

## 现场与约束

任务卡给定基线为 `HEAD=11a7aba`。审计期间共享工作区被其他会话推进到 `4384c95`；文件传输相关源码未见本审计改动。所有命令均在共享工作区只读执行（测试产生的构建缓存除外）。本报告是本轮唯一新增文件。

以下 Safe Flash 在途文件属于另一会话，整个审计期间未触碰：

```text
src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs
src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/safe_flash.rs
src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs
src/Nwflash.Desktop/src/app/App.tsx
src/Nwflash.Desktop/src/app/ipc-events.ts
src/Nwflash.Desktop/src/styles/components.css
src/Nwflash.Desktop/src/components/PartitionFailureDialog.tsx
src/Nwflash.Desktop/src/components/PartitionFailureDialog.test.tsx
```

## 测试结果总览

| 层级 | 命令 | 实际结果 |
|---|---|---|
| React 文件页 | `npm exec -- vitest run src/pages/FileManagerPage.test.tsx --run`（`src/Nwflash.Desktop`） | 1 file，11/11 passed |
| React 页面工厂 | `npm exec -- vitest run src/pages/FileManagerPage.test.tsx src/components/PageFactory.test.tsx` | FileManager 11/11 + PageFactory 3/3 passed |
| Rust application | `cargo test -p nwflash-application --test file_manager --test file_transfer -- --nocapture` | 6/6 + 4/4 passed |
| Rust application 内部文件名校验 | `cargo test -p nwflash-application --lib file_manager::tests -- --nocapture` | 2/2 passed |
| Rust Windows transport | `cargo test -p nwflash-windows --lib device_transport::tests -- --nocapture` | 15/15 passed |
| Rust Windows process | `cargo test -p nwflash-windows --lib process::tests -- --nocapture` | 33/33 passed（测试中预期的 fixture panic 日志不影响结果） |
| Operation coordinator | `cargo test -p nwflash-application --test operation_coordinator -- --nocapture` | 31/31 passed（含预期的 panic 捕获日志） |
| Tauri 编译 | `cargo test -p nwflash-tauri --lib --no-run` | exit 0，测试二进制成功生成 |
| Tauri 文件命令执行 | `cargo test -p nwflash-tauri --lib files::tests -- --list` | exit 1；测试二进制启动前 `0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`，6 个 files 测试未执行 |
| Desktop 类型/构建 | `npx tsc --noEmit`；`npm run build` | 均 exit 0；Vite 66 modules built |
| Native E2E | `src/Nwflash.Desktop/e2e-tests` | `node_modules` 与 e2e-native binary 均不存在，未能运行 WDIO；lockfile dry-run 可解析 |

## 按用户动作顺序的验证

### B1. 文件选择、React 状态与 IPC

#### 1. 选择本地文件或远端目标

- **实际路径**：`FileManagerPage.tsx` 的上传/安装分别调用 `open({ multiple:false, directory:false })`；下载调用 `save({ defaultPath: entry.name })`。
- **IPC 负载**：
  - 上传：`files_upload { sourcePath, remoteDirectory }`
  - 下载：`files_download { remotePath, destinationPath }`
  - 安装：`files_install_apk { apkPath }`
  - 列表：`files_list { remoteDirectory }`
  - 删除：`files_delete { remotePath }`
- **实际测试**：FileManagerPage 11/11 通过；覆盖两种下载入口、上传后刷新、APK 选择、目录进入/返回、删除确认。
- **预期**：取消系统文件对话框不应发 IPC。代码在 `typeof ... !== 'string'` 时直接返回，符合预期；但没有对应的自动化断言。
- **影响**：低（行为已有 fail-closed 分支，缺的是回归测试）。

#### 2. React 状态、按钮禁用与错误显示

- `hasAdbConnection` 只接受 `connection_state === 'AdbConnected'`；Fastboot/断开状态下文件操作按钮禁用。PageFactory 相关 3 个测试通过。
- `isRefreshingRemote` 会锁定刷新、目录进入、条目下载和删除；目录点击触发 `files_list`，删除先进入确认模态层。
- 传输/安装按钮没有绑定 `isRefreshingRemote` 或全局 `operations`，因此在一次操作进行时仍可再次点击；后端会以 coordinator `InProgress` 拒绝第二次调用，页面只显示错误。
- `selectedRemote` 在目录切换/刷新时不会自动清除；旧条目可能在列表刷新后继续作为工具栏操作对象，直到用户重新选择或删除成功。
- **影响**：中（并发点击和陈旧选择会造成错误提示或对用户造成误导）。

#### 3. IPC 边界

- 前端从不提交 serial；Tauri `build_*_plan` 从 `DeviceRuntime.active_adb_serial()` 取得当前 ADB serial，再构造命令。
- `src/Nwflash.Desktop/e2e-tests/specs/embedded-invoke.e2e.ts` 只验证 `files_list` mock 注册/调用账本和旧命令 `file_transfer_build_pull_command` 不存在；没有验证真实 Tauri serde 反序列化。
- **实际结果**：页面单测中的 invoke 参数断言通过；真实 Tauri files 命令测试因 loader 失败未执行。
- **影响**：中高（IPC 合同的运行时证据缺失）。

### B2. Tauri/应用层、ADB 参数与进程

#### 4. 登录、会话与 coordinator admission

- `files_list` 使用 `OperationKind::Discovering`；删除/上传/下载使用 `Transferring`；APK 安装使用 `Installing`。
- 所有执行路径进入 `OperationCoordinator::run_async`。Composite gate 先做本地保护检查，再做 Cloudflare 授权；无 token 会拒绝，401/426/完整性错误会阻断，网络/5xx 按现有 advisory 策略放行。
- `run_async` 建立单一 permit、保存取消令牌、写入开始/终态日志、记录 usage，并在完成时清理当前状态。coordinator 31/31 通过。
- 文件命令在构造计划前先读取 `DeviceRuntime`；非 ADB、空 serial 会在启动进程前失败。
- **缺口**：没有以注入式 process executor 跑过 `files_list/files_download/files_upload/files_delete/files_install_apk` 的完整 async 命令测试，无法证明 gate、spawn、终态在 files 命令自身中连通。
- **影响**：高（发布验收门缺少真实文件命令执行证据）。

#### 5. ADB push/pull/list/delete/install 参数

`FileManagerService` 与 `PlatformTools` 的计划构造结果如下，均为参数数组，不经过本地 shell：

```text
adb -s <serial> pull <remote-absolute-path> <local-absolute-path>
adb -s <serial> push <local-file> <remote-directory>/<basename>
adb -s <serial> shell "ls -laL -- '<quoted-directory>/'"
adb -s <serial> shell "rm -rf -- '<quoted-path>'"
adb -s <serial> install -r <apk-path>
```

- `file_manager` 6/6：验证无 `shell` 的 push/pull、当前 serial、远端 `..` 穿越拒绝、空 serial、空格/单引号 quoting、根目录不可删除、非 APK 拒绝、目录优先列表排序。
- `file_transfer` 4/4：验证 ADB Root `exec-out su --no-pty -c dd ...` 读取、设备路径校验、空 serial，以及直接 root 二进制写入必须改走 staging。该服务目前只有定义与测试引用，生产 UI `files_*` 走 `FileManagerService`。
- Windows transport 15/15：验证 staging push → root `dd` → cleanup 命令数组与路径组件拒绝。
- **覆盖缺口**：空/空白远端路径、缺失本地父目录、缺失源文件、Windows 保留文件名通过 `build_pull` 的完整组合，以及 malformed `ls` 行的更多变体没有单独端到端断言。
- **影响**：已覆盖路径为低风险；缺口为中（建议补测试，不需改变本轮产品范围）。

#### 6. stdout/stderr、退出码与进程收尾

- `execute_file_command` 和 `files_list` 都调用 `run_command_with_cancel(..., None, cancellation)`；Windows process runner 在子进程运行期间并行排空 stdout/stderr，输出上限 8 MiB，取消/超时请求 taskkill 树终止并在有限窗口内 reap。
- process 33/33（以及 Windows lib 58/58）通过：覆盖大 stdout、大 stderr、慢进程、输出超限、spawn/读管道失败、取消、超时、退出码和 reader reap，无管道死锁证据。
- 非零退出在 files 命令内暂时携带 stderr 构造 `DomainError::ExternalTool`，随后 coordinator 用公共错误文案写 snapshot/log 并返回；当前没有 files 专用测试确认这条映射。
- 文件命令只在开始时 `report_stage`，成功时一次性 `report_progress(1.0)`；没有解析 ADB 的字节进度。
- **影响**：进程基础设施低风险；files 集成输出/进度证据高缺失、中等用户体验问题。

### B3. 成功、失败、取消、partial 与重试

#### 7. 成功/空文件/非法输入/设备不可用/非零退出

- 计划层证明普通文件 push/pull 与 APK 过滤；UI mock 证明成功后的状态文案和上传后刷新。
- `validate_local_source` 使用 `Path::is_file()`，零字节普通文件理论上会被接受；没有零字节回归测试。
- `active_adb_serial` 对断开/Fastboot/空 serial fail-closed；没有 files command 专用调用测试。
- 没有可注入的 `adb` mock 来实跑 exit 0、exit 非 0、stderr 文案和解析结果，因此“成功/失败”在这一层仍属于静态推断。

#### 8. 开始前、传输中、完成后的取消

- process 层取消测试通过，coordinator `cancel_current` 测试通过；取消令牌确实传入进程 runner。
- **用户路径缺失**：`FileManagerPage` 没有“停止操作”按钮，也没有 `operation_cancel` 调用；全局 `AppShell`/`OperationProgressPanel` 只显示进度文本，不提供通用取消控件。`operation_cancel` 当前只被 QuickFlash、LineFlash、Firmware、Root、Resource 页面使用。
- 因此文件传输可在内部被取消，但用户在文件页无法主动取消长 push/pull/install，只能等待、关闭窗口或触发 coordinator 的其他收尾路径。
- **影响：高（P1）**，与阶段 B 的“可取消性”验收不符。

#### 9. 慢进程、大输出与永久 pending

- Windows process 33/33 覆盖慢进程、大输出、reader reap 和有限终止等待，未发现基础设施级永久 pending。
- 但 files 命令没有自己的注入测试；无法证明在 `OperationCoordinator` 持 permit 的真实 files closure 中同样完成清理。
- **影响**：中高（测试缺口；基础设施结果偏正面）。

#### 10. partial 文件清理、重复执行与安全重试

- `files_upload` 直接把 `adb push` 目标写入最终远端路径；`files_download` 直接把 `adb pull` 目标写入用户选定的最终本地路径。
- `execute_file_command` 在取消、非零退出、超时或进程树终止后没有删除远端目标、删除本地目标、改用临时名、校验长度或原子 rename 的逻辑。process runner 只负责进程，不负责文件回滚。
- 这意味着失败/取消可能留下远端半文件，或截断/覆盖本地已有文件；当前没有 partial 清理或失败后重试按钮。用户再次点击可重新发起完整命令，但不是受控的 resume/retry。
- **影响：高（P1）**，需在有权限的业务修复批次中优先处理；本轮只记录，未改源码。

### B4. 日志、快照、E2E mock 与验收门

#### 11. operation snapshot、进度与日志

- coordinator 会写开始/完成/取消/失败终态及 usage 记录；`files.rs` closure 只报告初始阶段和最终 `1.0`。
- `PageFactory` 给 FileManagerPage 传 `deviceSnapshot`，不传 `operationSnapshot`；文件页自己的“文件日志”只显示一条本地成功/错误文案。
- `App.resolveBusyKind` 将 `OperationKind::Transferring` 映射到 `lineFlash` 通道、`Installing` 映射到 `safeFlash` 通道，所以全局状态栏会显示“可视刷写/ VIVO 线刷”而非文件传输/安装 APK；`App.test.ts` 目前正是按这一映射断言通过。
- **影响**：中（P2，状态可见但语义错误、无字节进度）。

#### 12. 现有 E2E mock 的实际边界

- `interactions.e2e.ts` 只有一个文件动作：mock `files_list`/`files_delete`，模拟 ADB snapshot → 文件页 → 删除确认 → 删除成功文案。
- `embedded-invoke.e2e.ts` 只做 `files_list` 注册/参数账本和旧命令 not-found 检查。
- `direct-mock-bridge` 在 WebView 内拦截 invoke；这些用例不会进入 Rust/Tauri `files.rs`、coordinator、adb 或 process runner。`VISUAL_STATE_FIXTURES.fileEntries[0].full_path` 甚至是 `device-file-1`，因 mock 绕过后端验证不会暴露问题。
- e2e-tests 没有 `node_modules`，专用 `target/e2e-native` binary 也不存在；lockfile `npm ci --dry-run --offline` 可解析依赖，但本轮未安装或运行 WDIO。
- **影响**：高（B5“至少一轮端到端 mock 成功/失败/取消/重试”未满足）。

## 值得修复 / 可忽略筛选

### 值得修复（按优先级）

1. **FT-P1-01：文件传输取消入口**。在文件页或全局操作栏提供 `operation_cancel`，并补开始前/传输中/完成后取消测试；保持现有 coordinator token 语义。
2. **FT-P1-02：partial/原子文件策略**。push/pull 使用受控临时目标，成功后 rename/promote；取消、非零、超时、spawn 失败时清理 partial，且不破坏已有本地文件。补可注入 process/files 命令测试。
3. **FT-P1-03：Tauri files 执行测试 seam**。为 `files.rs` 注入 `ProcessExecutor` 或等效 mock，验证 gate → spawn → stdout/stderr → exit code → snapshot/log → permit release 的真实链路；同时在带 Common Controls v6 manifest 的环境解开当前 `0xc0000139` 阻断。
4. **FT-P2-04：进度与状态标签**。让 files closure 发出可观测阶段/字节进度，或明确“不支持字节进度”的 UX；修正 `Transferring`/`Installing` 的全局通道标签，并将 FileManager 接入 busy/operation snapshot。
5. **FT-P2-05：状态竞态**。刷新/进入目录时清理不再存在的 `selectedRemote`；上传/安装/删除/下载按钮在全局 busy 或本页操作期间禁用，避免 coordinator 拒绝和陈旧目标。
6. **FT-P2-06：E2E mock 扩展与依赖安装**。在 `e2e-tests` 完整安装锁定依赖后，至少加入 push、pull、install 的成功/失败/取消/重试用例；明确哪些是 UI mock、哪些真正进入 Rust。

### 本轮可忽略或明确留待产品裁决

- 哈希校验与跨步骤 serial 绑定：阶段 B 明确排除，不能借本轮扩展范围。
- 真机行为：当前主机无真机，使用 mock/录制输出即可；真机 smoke 另列授权门。
- `FileTransferService`（ADB Root 分区 helper）若确认仅为历史 API，可单独做生命周期清理；不应与普通文件页修复混在一起。
- 罕见 Unicode/负文件大小等 parser 边界可作为低优先级测试补齐，当前没有复现的用户影响。

## 后续独立派工卡

| 卡片 | 责任范围 | 禁止触碰 | 验收 |
|---|---|---|---|
| FT-P1-01 | FileManager UI + operation_cancel 集成 | Safe Flash 8 文件 | 长传输可取消，coordinator 终态为 Canceled，UI 解锁 |
| FT-P1-02 | files.rs 与文件临时/原子收尾 | Safe Flash 8 文件 | 成功 promote；失败/取消无 partial 且旧文件保留 |
| FT-P1-03 | Tauri files 测试注入 seam + Windows manifest 环境 | Safe Flash 8 文件 | files 6 个测试实际运行；完整 async mock 链通过 |
| FT-P2-04 | 进度、状态标签、operation snapshot 投射 | Safe Flash 8 文件 | 文件操作显示准确标题/阶段/进度 |
| FT-P2-05 | FileManager 状态竞态与边界测试 | Safe Flash 8 文件 | 刷新/目录切换/重复点击无陈旧目标或 InProgress 噪音 |
| FT-P2-06 | e2e-tests 依赖与 mock 场景 | 全部业务源码（先只补测试） | WDIO mock 至少覆盖 push/pull/install 的成功、失败、取消、重试 |

## 本轮修改与回退

- **修改文件**：仅新增 `docs/2026-09-04-file-transfer-validation.md`。
- **源码修改**：无。
- **提交**：无（报告留在工作树，等待总指挥按报告/测试/diff 一致性统一提交）。
- **回退**：删除本报告文件即可；不应使用 reset/clean，也不应影响其他会话的工作树或 Safe Flash 在途文件。
