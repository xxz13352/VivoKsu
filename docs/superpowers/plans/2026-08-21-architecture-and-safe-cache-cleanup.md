# 项目架构与安全构建缓存清理实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**目标：** 定向清理 Tauri debug 缓存和 E2E 日志，同时把项目级架构文档更新为当前源码的事实基线。

**架构：** 清理使用 Cargo 的 `dev` profile 组件化清理，以保留同一 target 下的 release 产物；E2E 日志则仅删除已解析、且在固定目录中的 `.log` 文件。架构文档以 React、Tauri command 边界、Cargo 依赖方向和当前 DTO/runtime 为事实源，对高层摘要做最小一致性修正。

**技术栈：** PowerShell、Cargo 1.97+、Rust 2021、Tauri 2、React/TypeScript、Markdown。

## 全局约束

- 只删除 `src/Nwflash.Desktop/src-tauri/target/debug` 的 Cargo `dev` profile 产物和 `src/Nwflash.Desktop/e2e-tests/logs/*.log`。
- 完整保留 `target/release-rebuild/`、`src/Nwflash.Desktop/src-tauri/target/release/`（包括 `bundle`、`nsis`、`resources` 和 EXE）、`node_modules/`、`dist/`、`src-tauri/gen/`、`.superpowers/`、`artifacts/`、资源和所有其他源文件。
- 不对仓库根目录、用户主目录、`%TEMP%` 或未解析的 glob 执行递归删除。
- 每个删除目标必须在执行前解析为工作区内的绝对路径；若 Cargo dry-run 出现 `target\\release` 路径，立即停止。
- 不删除或改写 Cloudflare、Rust/TypeScript 产品代码、平台工具二进制、签名、安装器、真机或发布产物。
- `docs/project-architecture.md`、`docs/index.md`、`docs/architecture-tauri-migration.md` 和 `src/Nwflash.Desktop/docs/rust-tauri-architecture.md` 均存在用户工作区更改或未追踪内容；不暂存、不提交它们。
- 架构文档必须如实区分“源码已实现”与“仍需外部验收”；不声称真机、WDIO native、签名、安装器或发布已完成。

---

### Task 1: 验证并清理 Tauri Debug 缓存和 E2E 日志

**文件/目录：**
- 删除：`src/Nwflash.Desktop/src-tauri/target/debug/`
- 删除：`src/Nwflash.Desktop/e2e-tests/logs/*.log`
- 保留：`target/release-rebuild/release/nwflash-desktop.exe`
- 保留：`src/Nwflash.Desktop/src-tauri/target/release/{bundle,nsis,resources,nwflash-desktop.exe}`

**接口：**
- 消费：`Cargo.toml` 中的 `dev` profile 和 E2E 日志固定目录。
- 产出：不影响 release 产物的本地空间回收，且日志目录中不留 `.log` 文件。

- [ ] **Step 1: 解析并冻结删除边界。**

运行：

```powershell
$workspace = (Resolve-Path -LiteralPath '.').Path
$debugTarget = (Resolve-Path -LiteralPath 'src\Nwflash.Desktop\src-tauri\target\debug').Path
$logRoot = (Resolve-Path -LiteralPath 'src\Nwflash.Desktop\e2e-tests\logs').Path
$releaseExe = Resolve-Path -LiteralPath 'target\release-rebuild\release\nwflash-desktop.exe'
$tauriRelease = Resolve-Path -LiteralPath 'src\Nwflash.Desktop\src-tauri\target\release'
if (-not $debugTarget.StartsWith($workspace, [StringComparison]::OrdinalIgnoreCase)) { throw 'Debug target escapes workspace.' }
if (-not $logRoot.StartsWith($workspace, [StringComparison]::OrdinalIgnoreCase)) { throw 'Log target escapes workspace.' }
Get-ChildItem -LiteralPath $logRoot -File -Filter '*.log' | Select-Object -ExpandProperty FullName
```

预期：只出现 `target\\debug` 和 `e2e-tests\\logs` 下的绝对路径，release EXE 和 Tauri release 目录存在。

- [ ] **Step 2: 以 Cargo dry-run 证明不会删 release。**

运行：

```powershell
$dryRun = cargo clean --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --profile dev --dry-run --verbose 2>&1
if ($LASTEXITCODE -ne 0) { throw 'Cargo dry-run failed.' }
if ($dryRun | Select-String -Quiet '\\target\\release\\') { throw 'Dry-run includes a release path; abort cleanup.' }
if (-not ($dryRun | Select-String -Quiet '\\target\\debug\\')) { throw 'Dry-run did not identify the debug target; abort cleanup.' }
```

预期：命令成功，只标识 `target\\debug` 产物，不包含 `target\\release` 路径。

- [ ] **Step 3: 运行 profile-scoped Cargo 清理。**

运行：

```powershell
cargo clean --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --profile dev
```

预期：Cargo 移除 dev 编译产物；不使用 `Remove-Item` 或仓库级删除。

- [ ] **Step 4: 删除经冻结的 E2E 日志文件。**

仅对 Step 1 列出的五个绝对路径使用 `apply_patch` 的 `*** Delete File:` 操作：

```text
src/Nwflash.Desktop/e2e-tests/logs/wdio-2026-08-19T11-15-25-253Z.log
src/Nwflash.Desktop/e2e-tests/logs/wdio-2026-08-19T11-23-34-803Z.log
src/Nwflash.Desktop/e2e-tests/logs/wdio-2026-08-19T11-25-52-751Z.log
src/Nwflash.Desktop/e2e-tests/logs/wdio-2026-08-19T11-27-20-782Z.log
src/Nwflash.Desktop/e2e-tests/logs/wdio-2026-08-19T11-37-10-249Z.log
```

不删除 `logs` 父目录，以免改变 WDIO 运行期期望。

- [ ] **Step 5: 验收清理结果。**

运行：

```powershell
if (Test-Path -LiteralPath 'src\Nwflash.Desktop\src-tauri\target\debug') { throw 'Debug target still exists.' }
if (Get-ChildItem -LiteralPath 'src\Nwflash.Desktop\e2e-tests\logs' -File -Filter '*.log' -ErrorAction SilentlyContinue) { throw 'E2E log files remain.' }
if (-not (Test-Path -LiteralPath 'target\release-rebuild\release\nwflash-desktop.exe')) { throw 'Preserved release executable is missing.' }
foreach ($preserved in @('src\Nwflash.Desktop\src-tauri\target\release\bundle','src\Nwflash.Desktop\src-tauri\target\release\nsis','src\Nwflash.Desktop\src-tauri\target\release\resources')) {
  if (-not (Test-Path -LiteralPath $preserved)) { throw "Preserved release path is missing: $preserved" }
}
```

预期：调试缓存和所有指定日志消失，发布 EXE/bundle/NSIS/resources 仍在。

- [ ] **Step 6: 不暂存或提交被清理的未追踪目录。**

运行：

```powershell
git diff --cached --name-only
git status --short -- src/Nwflash.Desktop/src-tauri/target src/Nwflash.Desktop/e2e-tests/logs
```

预期：没有暂存内容；清理结果只作为本地工作区状态报告。

### Task 2: 写入当前项目架构与修正高层 serial 漂移

**文件：**
- 修改：`docs/project-architecture.md:1-216`
- 修改：`docs/index.md:65-69`
- 修改：`docs/architecture-tauri-migration.md:541-547`
- 修改：`src/Nwflash.Desktop/docs/rust-tauri-architecture.md:46-83`

**接口：**
- 消费：`Cargo.toml` workspace 依赖、`AppState`、`generate_handler!` 注册表、`DeviceSnapshot`、`DeviceSnapshotPayload`、Quick Flash/ROOT 设备复核代码和 React `App.tsx` 调用边界。
- 产出：以 `docs/project-architecture.md` 为唯一项目级规范，且三份高层摘要与它一致。

- [ ] **Step 1: 冻结源码事实。**

运行：

```powershell
rg -n 'pub struct AppState|pub struct DeviceSnapshot|serial: String|interface DeviceSnapshotPayload|verify_execution_device|take_automatic_for_device|get_for_device|generate_handler!' src/Nwflash.Desktop/src-tauri src/Nwflash.Desktop/src/app/ipc-events.ts
```

预期：证明 `AppState` 是 runtime 所有者，设备 DTO 包含用于展示的 serial，Quick Flash 与 ROOT 高风险流程在执行前拒绝设备变更，且公开 command 只来自 `generate_handler!`。

- [ ] **Step 2: 更新规范项目架构文档。**

在 `docs/project-architecture.md` 保留现有系统图和分层责任，将文档状态更新为 `2026-08-21`，并用以下事实替换旧的 serial 声明：

```text
DeviceSnapshot 和 TypeScript DeviceSnapshotPayload 包含 serial 供界面展示；浏览器不能提交、选择或伪造执行 serial。Rust 只从当前 DeviceRuntime 派生 ADB/Fastboot 命令目标。Quick Flash 和部分 ROOT 高风险流程会在预检/工件消费前复核当前 serial，设备变更时拒绝继续。
```

明确记录如下架构事实：

```text
React -> Tauri invoke/event -> nwflash-tauri -> nwflash-application
                                                    |-> nwflash-infrastructure
                                                    |-> nwflash-windows
nwflash-domain 被 windows/infrastructure/application/tauri 共享
```

文档中必须清楚列出 `AppState` runtime、`OperationCoordinator`、不透明工件/capability、公开 command 白名单、资源 integrity/staging、React 显示责任、Cloudflare 边界和仍需外部验收的项目。不删除 WPF 历史/视觉基线边界，不把源码存在误写为真机或发布已验收。

- [ ] **Step 3: 同步三份高层摘要。**

将 `docs/index.md:67`、`docs/architecture-tauri-migration.md:543` 和 `src/Nwflash.Desktop/docs/rust-tauri-architecture.md:53,59` 中“serial 不进入前端 DTO”、“不做预检/执行等值绑定”的概括，改为 Step 2 中的准确事实。保留下列不变的安全约束：

```text
浏览器不能提交任意程序、命令数组、shell 文本或未校验的资源路径；token、ROM URL、staging 路径和预检工件仍留在 Rust 边界。
```

不修改 `root_ota_extract_images` 的特定 runtime 规则；它依据已验证的 ROOT OTA 来源运行，不是此高层 serial 展示/预检规则的反例。

- [ ] **Step 4: 验证文档事实与范围。**

运行：

```powershell
$legacy = rg -n -i 'serial 不进入前端|not enter.*frontend|no serial.binding|不以 serial 绑定或比较跨步骤身份' docs/index.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/docs/rust-tauri-architecture.md docs/project-architecture.md
if ($LASTEXITCODE -eq 0) { $legacy; throw 'Stale serial summary remains.' }
if ($LASTEXITCODE -ne 1) { throw 'Serial summary scan failed.' }
git diff --check -- docs/project-architecture.md docs/index.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/docs/rust-tauri-architecture.md
git diff -- docs/project-architecture.md docs/index.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/docs/rust-tauri-architecture.md
```

预期：旧概括不再出现，没有 whitespace error，并且定向 diff 只包含架构事实修正。

- [ ] **Step 5: 不暂存或提交共享工作区文档。**

运行：

```powershell
git diff --cached --name-only
git status --short -- docs/project-architecture.md docs/index.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/docs/rust-tauri-architecture.md
```

预期：没有暂存内容；文档修改留在共享工作区中，供用户现有迁移提交统一管理。

### Task 3: 记录清理结果与架构验收边界

**文件：**
- 修改：`docs/project-architecture.md: 临时文件与生命周期章`

**接口：**
- 消费：Task 1 的定向清理结果和 Task 2 的实现/外部验收分类。
- 产出：细化过的本地清理指南，不把构建缓存与发布物混为一谈。

- [ ] **Step 1: 用实际路径替换过度宽泛的清理描述。**

在 `docs/project-architecture.md` 的“临时文件与生命周期”章追加此规则：

```text
当前深度清理只针对 src-tauri/target/debug 和 e2e-tests/logs/*.log。target/release-rebuild 与 src-tauri/target/release（含 bundle、nsis、resources 和 EXE）属于保留范围，不能用库级 wildcard 删除。node_modules、dist、gen 和 .superpowers 可重建或属于本地状态，但不在本次默认清理范围。
```

同时保留现有“不对仓库根、用户目录或 `%TEMP%` 做递归删除”的约束。

- [ ] **Step 2: 执行最终文档和本地状态检查。**

运行：

```powershell
git diff --check -- docs/project-architecture.md docs/index.md docs/architecture-tauri-migration.md src/Nwflash.Desktop/docs/rust-tauri-architecture.md
rg -n 'target/release-rebuild|target/release|target/debug|e2e-tests/logs' docs/project-architecture.md
git status --short
```

预期：文档无 whitespace error，清理边界清楚，且用户原有的工作区更改仍被保留。

- [ ] **Step 3: 只提交计划自身的文件（如已单独提交则不重复提交）。**

确认 `docs/superpowers/specs/2026-08-21-architecture-and-safe-cache-cleanup-design.md` 仅包含设计文档提交 `74b1a2f`。不暂存任何用户所有的未追踪产品文件或文档。
