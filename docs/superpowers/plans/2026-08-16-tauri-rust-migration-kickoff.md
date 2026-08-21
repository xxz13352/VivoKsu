# Tauri/Rust 迁移执行计划（继续执行版）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 按 `docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md` 与 `docs/migration-baselines/api-contract-cases.md`，把 5.3codex 继续推进为可复现的 Tauri/NWflash 迁移执行序列；保持 UI 与行为不大改，且不改 `cloudflare/**`。

**Architecture:** 继续沿用现有 5-crate 分层（`domain / application / infrastructure / windows / tauri`）与 React 壳体结构；先做“行为等价”，再补功能动作通道。前端仅渲染状态，不持久化 token，不拼装任意 shell 命令，不直接访问网络。

**Tech Stack:** Rust (Tokio/Reqwest/Serde/Windows), Tauri, React + TypeScript + Vite, Vitest + Rust test。

## 给 5.3codex 的首批执行序列（现在就可开工）

### 第1步：先冻结基线（不改代码）

- [ ] 验证以下文件已存在且无外部依赖变更：
  - `docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md`
  - `docs/migration-baselines/api-contract-cases.md`
  - `docs/superpowers/plans/2026-08-16-tauri-rust-migration-kickoff.md`
- [ ] 记录偏差：`src/Nwflash.Desktop/src/app/pageManifest.ts` 当前 `OperationLog` 处于导航组中，需在 Task 1 调整为右侧状态区日志页（不做左侧按钮）。
- [ ] 标记本次执行“无 cloudflare 与 web 修改边界”。

### 第2步：5.3codex 继续执行任务切片

1. `Task 1`（壳体收口）：先补齐 10 项左侧导航顺序与统一进度/登出禁用行为、时钟和布局测试，通过 `npm --prefix src/Nwflash.Desktop run test -- AppShell window-state`。
2. `Task 2`（API 契约）并行卡住 UI 逻辑：先把 API 合约测试写完并绿灯后，再给 Rust command 提供可注入 token 与版本头。
3. `Task 3`（会话门禁）：把现有 `OperationCoordinator` 与 `Heartbeat` 语义（强制更新、force_exit、goodbye、忙态拒绝）翻到 Rust，先独立跑 `cargo test`。

### 里程碑停机点（每完成一项先提PR）

- 里程碑A：Task1 全绿且 `docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md` 未修改。
- 里程碑B：Task2 全绿（API 测试 + Rust 契约桥）。
- 里程碑C：Task3 全绿（会话门禁 + 进度快照 + 安全退出）。
- 只有 C 通过后再进 Task4（刷写/传输链路接口面）防止边界漂移。

## Global Constraints

- 先实现 `cloudflare` 契约等价，不修改任何 `cloudflare/**`。
- `UpdateRequiredException` 仍为无跳过路径；426 一律更新退出。
- 页面顺序与文案固定（10 导航项 + 右侧操作日志区）。
- `logout` 在 `OperationCoordinator.IsBusy || DeviceSession.IsBusy` 时禁用。
- 不用计划性大改 UI；不引入第三方组件库重构。

---

### Task 0: 基线接力与验收脚手架冻结

**Files:**
- Create: `docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md`
- Create: `docs/migration-baselines/api-contract-cases.md`

**Consumes:** `App.xaml.cs`、`MainWindow.xaml`、`OperationCoordinator.cs`、`HeartbeatService.cs`、`OtaApiClient.cs`、`AppVersionService.cs`、`LoginService.cs`、`OperationLogService.cs`、`cloudflare/API.md`

**Produces:** 统一对照文件，作为 5.3codex 的唯一迁移源，先不允许再调整设计假设。

- [ ] **Step 1: 按这次基线文档锁定“最小可接受行为”**
  - 统一进度优先级、导航顺序、退出禁用规则、更新/心跳/登录链路。
- [ ] **Step 2: 记录差异清单**
  - 标记前端 `src/Nwflash.Desktop` 当前壳体与 WPF 基线偏差（如 `OperationLog` 页面入口是否保留）。
- [ ] **Step 3: 形成执行序号**
  - 任务流从 `Task3`（壳体）延伸到 `Task19` 的顺序冻结后再开工。

### Task 1: 完成 Tauri 壳体的行为等价与测试闭环（Task3 收口）

**Files:**
- Modify: `src/Nwflash.Desktop/src/app/pageManifest.ts`
- Modify: `src/Nwflash.Desktop/src/app/AppShell.tsx`
- Modify: `src/Nwflash.Desktop/src/app/window-state.ts`
- Modify: `src/Nwflash.Desktop/src/components/Sidebar.tsx`
- Modify: `src/Nwflash.Desktop/src/app/App.tsx`
- Tests: `src/Nwflash.Desktop/src/components/AppShell.test.tsx`, `src/Nwflash.Desktop/src/app/window-state.test.ts`

**Consumes:** `docs/migration-baselines/2026-08-16-wpf-behavior-baseline.md`

**Produces:** 与 WPF 视觉/交互等价的壳体，带可复现测试基线。

- [ ] **Step 1: 写失败测试**
  - 导航顺序（10项） + `OperationLog` 非主导航 + 左右列布局比例 + 刷新时钟格式 + 退出禁用条件。
- [ ] **Step 2: 运行并确认失败**
  - `npm --prefix src/Nwflash.Desktop run test -- AppShell window-state`
- [ ] **Step 3: 对齐壳体状态**
  - 左侧保持 10 项主 nav；右侧统一进度显示只保留一条文案；空闲态为“无进行中的操作”。
- [ ] **Step 4: 运行并通过壳体测试**
- [ ] **Step 5: 只做壳体与 UI 对齐提交**
  - 不迁移业务逻辑，不改外部命令/网络。

### Task 2: 云端会话与版本门禁（Task5 前置）

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/{api_client.rs,api_model.rs}`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/{auth.rs}`
- Tests: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/*`

**Consumes:** `docs/migration-baselines/api-contract-cases.md`

**Produces:** Rust API 客户端命令，覆盖 `/api/app/version`、`/api/login`、`/api/me`、`/api/heartbeat`、`/api/online`、`/api/operation/authorize`、`/api/usage/logs`、`/api/rom`。

- [ ] **Step 1: 写失败 API 测试**
  - 断言版本头、Bearer header、pd/version 编码、426 映射、401/403 退出、非 JSON 426/403 解析安全。
- [ ] **Step 2: 运行失败用例**
  - `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test api_contract`
- [ ] **Step 3: 实现 API 合约**
  - 仅在 Rust 中做 contract；登录令牌只在会话状态保留。
- [ ] **Step 4: 通过 API 测试**
- [ ] **Step 5: 暂停（不做设备刷写），提交 API 基线**

### Task 3: 会话门禁与退出/强退桥接（Task6）

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/operation_coordinator.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/session_lifecycle.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/session.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/{operation_coordinator.rs, session_lifecycle.rs}`

**Consumes:** WPF 的 `OperationCoordinator.cs` + `HeartbeatService.cs` + `AppComposition` 会话流程。

**Produces:** 单点运行门禁、可取消会话、goodbye/force_exit/update_required 路径，供后续刷写页复用。

- [ ] **Step 1: 写失败测试**
  - 并发拒绝、取消传播、usage 记录、日志频率节流、force_exit 回调顺序。
- [ ] **Step 2: 运行失败并确认无实现漏项**
- [ ] **Step 3: 实现协调器与会话事件桥接**
  - 与 WPF 行为一致（无排队、拒绝即反馈、进度节流、退出前最少一次 goodbye）。
- [ ] **Step 4: 通过测试并在前端订阅 `operation` 事件**
  - 验证 `AppShell` “注销禁用”与“统一进度文案”联动。

### Task 4: 刷写/传输/读取链路的最小接口面（Task8~Task12 开始）

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/src/{process.rs,platform_tools.rs,device_transport.rs}`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/quick_flash.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/file_transfer.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/{quick_flash.rs, file_transfer.rs}`

**Consumes:** WPF 服务层（`FastbootCliRunner`、`AdbRootTransferRunner`、`MirrorService`、`FileManagerViewModel`）对应行为。

**Produces:** Rust 原生参数化进程执行与端到端测试桩，供后续页面接入。

- [ ] **Step 1: 写失败测试（先不写 UI）**
  - 参数边界（串口/路径/取消）、环境变量 `ADB`、进程终止行为。
- [ ] **Step 2: 实现命令执行最小层**
  - 不拼接命令行字符串，使用数组参数执行；失败时保留上下文与错误码。
- [ ] **Step 3: 通过测试并固定事件 contract**
- [ ] **Step 4: 切入前端页面映射（仅一页）**
  - 先对齐“可视刷写/文件管理/镜像日志”三类最小面向交互，再继续扩展。

### Task 5: 分页面功能迁移与验收冻结（Task12+）

**Files:** 按任务拆成独立子计划，不再在同一任务混写
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/{firmware_extract.rs, safe_flash.rs, root.rs}`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/{quick_flash.rs, firmware.rs, safe_flash.rs, root.rs}`
- `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/{payload_dumper.rs, firmware_package.rs, remote_assets.rs, resources.rs}`
- `src/Nwflash.Desktop/src/pages/{对应页}`

**Consumes:** 阶段前 4 步通过测试结果 + WPF 页面行为基线。

**Produces:** 页面级功能逐步可用（快速刷写、可视刷写、固件提取、VIVO 线刷、ROOT、软件/在线）。

- [ ] **Step 1: 每个子域先写 1-2 个端到端失败用例，再实现**
- [ ] **Step 2: 实现后本地脚本验收 + 截图对照（不允许新增按钮/文案）**
- [ ] **Step 3: 逐页确认“映射到快速刷写/跨页状态切换”行为一致**

### Task 6: 发布与保护交付前的冻结

**Files:**
- `scripts/Publish-Release.ps1`（或等价流程）
- `scripts/Protect-NwflashRelease.ps1`
- `scripts/Sign-NwflashRelease.ps1`

**Consumes:** 全量测试通过、Tauri + Rust + 前端测试通过。

**Produces:** 包含 VMP/签名、无 .NET runtime 依赖、NSIS 打包、安装/退出 smoke 的可复现发布链。

- [ ] **Step 1: 先做非保护发布通过性验证**
- [ ] **Step 2: 再加入 VMP 与签名钩子**
- [ ] **Step 3: 保护后安装/运行 smoke 与回归脚本通过**

## 执行建议（下一步动作）

1. 先把 `Task 0/1` 做成 5.3codex 的第一批提交（壳体与基线）
2. `Task2 + Task3` 一起作为“会话与安全”主线，优先级高于设备功能
3. `Task4/5` 按“一个页面一轮”推进，避免一次性引入大功能造成行为回归
4. 每个子任务都要先给失败测试，再最小实现，再测试通过，再提交
