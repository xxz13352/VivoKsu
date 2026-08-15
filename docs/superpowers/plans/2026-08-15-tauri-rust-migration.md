# 奶蛙Flash桌面端 Tauri/Rust 迁移实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将当前 WPF 桌面端迁移为保留现有视觉和业务行为的 Tauri v2 应用：React/TypeScript 复刻 UI，Rust 负责设备、固件、会话和授权逻辑，最终 Windows 发布物接入 VMP 与代码签名。

**Architecture:** 在 `src/Nwflash.Desktop/` 建立独立 Tauri 工程，旧 WPF 工程在整个迁移和真机验收期只读保留。前端只渲染状态和发送受控意图；所有设备命令、文件路径、Cloudflare 请求、操作门禁、取消与进度都由 Rust command/use case 执行。`cloudflare/**` 是外部兼容依赖，绝不修改。

**Tech Stack:** Tauri v2、React、TypeScript、Vite、Rust stable（`x86_64-pc-windows-msvc`）、Tokio、Reqwest、Serde、Windows crate、Vitest、Rust test、NSIS、VMProtect、Authenticode。

## 全局约束

- 用户可见名称只能是 **“奶蛙Flash”**；技术名称使用 `NWflash`/`NWF`，保留 `X-Nwflash-Version`。
- 仅迁移桌面端：`src/VivoKsu.App/**`、`src/VivoKsu.Bootstrapper/**`、桌面测试和桌面发布/验证脚本。**不得修改 `cloudflare/**`。**
- 不以客户端防护作为授权边界。VOTA 凭据继续只在 Worker；token 仅存 Rust 进程内存；每个会产生设备副作用的命令必须经 Cloudflare 许可。
- UI 必须视觉和交互保真：11 个页面、现有菜单顺序、中文文案、teal 主题、统一进度区、登录/更新/驱动/资源模态窗都不得重新设计。
- 外部二进制和资源（adb、fastboot、scrcpy、payload_dumper、APK、驱动、magiskboot）继续使用现有文件或下载来源；不得改写为 Rust。
- Rust 进程调用必须使用参数数组，禁止 `cmd.exe`/PowerShell 字符串拼接设备命令；取消必须结束整个进程树。
- 前端不得直连 `api.nwflash.cc.cd`，不得保存 token，不得获得通用 shell、通用文件读写或通用 HTTP 权限。
- 每个任务先写失败测试、确认失败、实施最小变更、确认通过，再单独提交。本文按项目所有者要求不包含实现源码；旧源码、指定接口和测试名称是实现依据。
- 发布物只支持 Windows x64；安装器为每用户 NSIS，WebView2 使用 `embedBootstrapper`；USB 驱动安装仍按需提权。
- VMP 只保护保护后烟测通过的少量 Rust 原生函数。对保护后的 EXE 签名，再签名最终 NSIS 安装包；私钥、证书密码、VMP 许可证和生产 token 不得进入仓库。

## 任务依赖

`1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9` 是基础链路。任务 10、11、12 可在任务 9 完成后顺序实施；任务 13、14、15、16 依赖相应设备/基础设施能力；任务 17、18、19 收尾并依次执行。任何任务失败都不跳过验证进入下一任务。

## 目标文件结构

| 目标路径 | 职责 | 主要旧源码依据 |
| --- | --- | --- |
| `src/Nwflash.Desktop/src/` | React 页面、设计系统、IPC client、前端测试 | `MainWindow.xaml`、各窗口 XAML |
| `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/` | 无 IO 模型、纯策略、错误分类 | `Models/`、`PartitionRiskPolicy`、`SafeFlashSlotPlanner` |
| `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/` | 操作协调、会话、设备监视、业务 use case | `OperationCoordinator`、`AppComposition`、ViewModels |
| `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/` | API、下载、解压、资源、偏好、日志 | `OtaApiClient`、下载/提取/provision 服务 |
| `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/` | 子进程、Windows IO、ADB/Fastboot、路径 | `FastbootCliRunner`、ADB/驱动服务 |
| `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/` | Tauri command、事件、窗口和应用状态 | `App.xaml.cs`、`AppComposition` |
| `src/Nwflash.Desktop/e2e-tests/` | WebdriverIO + `@wdio/tauri-service` 端到端 UI 验收 | `scripts/verify-*.ps1`、Task 1 截图 |
| `scripts/` | 构建、保护、签名、打包、验收 | `Publish-Release.ps1`、`verify-*.ps1` |

---

### Task 1: 锁定 WPF 行为、接口与视觉基线

**Files:**
- Create: `docs/migration-baselines/2026-08-15-wpf-behavior-baseline.md`
- Create: `docs/migration-baselines/screenshots/`（登录、主窗与所有页面的基线截图）
- Create: `docs/migration-baselines/api-contract-cases.md`
- Read only: `README.md`、`docs/architecture.md`、`docs/safeflash-ota.md`、`cloudflare/API.md`、全部 `tests/VivoKsu.App.Tests/*Tests.cs`

**Consumes:** 当前已通过的 WPF 测试、现有 UI 自动化脚本和真实 `cloudflare/API.md`。

**Produces:** 后续所有任务引用的页面状态矩阵、行为测试清单和 API 请求/响应/错误码矩阵；这些文件只记录事实，不改变产品代码。

- [ ] **Step 1: 编写基线清单**：按 11 个页面及 4 个模态窗记录可见文案、初始状态、加载状态、失败状态、运行中状态、可用按钮与禁用按钮；记录右上统一进度区五种显示优先级。
- [ ] **Step 2: 确认旧测试基线通过**：运行 `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`；预期为 0 个失败。将测试总数和命令输出时间写入基线文档。
- [ ] **Step 3: 捕获视觉基线**：使用现有 `scripts/verify-*.ps1`、已有截图能力或手工可重复步骤捕获所有页面；文件名采用 `<page>-<state>.png`，例如 `safe-flash-confirm.png`、`login-failed.png`。
- [ ] **Step 4: 记录 API 契约用例**：从 `cloudflare/API.md` 与 `OtaApiClientTests.cs`/`AppVersionControlTests.cs` 提取登录、ROM、心跳、在线、操作许可、使用日志、版本检查的成功与失败场景，明确 401、402、403、404、426、429 的中文含义。
- [ ] **Step 5: 自检并提交**：确认基线中没有 token、账号、设备序列号、真实 ROM URL 或本地绝对用户路径；提交 `docs: capture WPF migration baseline`。

### Task 2: 建立受限的 Tauri 工作区与可重复构建

**Files:**
- Create: `src/Nwflash.Desktop/package.json`、`package-lock.json`、`vite.config.ts`、`tsconfig.json`
- Create: `src/Nwflash.Desktop/src-tauri/Cargo.toml`、`Cargo.lock`、`build.rs`、`tauri.conf.json`
- Create: `src/Nwflash.Desktop/src-tauri/capabilities/default.json`
- Create: `src/Nwflash.Desktop/src-tauri/.cargo/config.toml`
- Create: `src/Nwflash.Desktop/README.md`
- Test: `src/Nwflash.Desktop/src-tauri/tests/build_smoke.rs`

**Consumes:** Task 1 的平台范围与 UI 基线。

**Produces:** 可在 Windows x64 构建并显示空白“奶蛙Flash”窗口的 Tauri 工程；Cargo/Node 依赖已锁定。

- [ ] **Step 1: 写失败构建/配置测试**：在 `build_smoke.rs` 检查编译期应用标识为 `nwflash`、产品名为“奶蛙Flash”、目标为 Windows x64，并检查 release 配置不包含 `.NET` 运行时依赖。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test build_smoke`；预期为 manifest 或测试目标不存在。
- [ ] **Step 3: 创建最小工作区**：建立标准 Tauri v2 + React/TypeScript/Vite 结构；创建五个空 Rust crate（domain、application、infrastructure、windows、tauri）并只建立显式依赖方向：`tauri → application → domain`，`application → infrastructure/windows`，前端不依赖 Rust crate。启用严格 CSP、生产环境禁用开发工具、仅声明窗口/对话框/外部链接所需 capability；禁止 shell、任意文件系统和任意 HTTP 插件权限。
- [ ] **Step 4: 运行构建测试确认通过**：运行 `npm --prefix src/Nwflash.Desktop ci`、`npm --prefix src/Nwflash.Desktop run build`、`cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --test build_smoke` 和 `npm --prefix src/Nwflash.Desktop run tauri -- build -- --no-bundle`；预期为四项成功并生成 Windows x64 可执行文件。
- [ ] **Step 5: 提交**：提交 `chore: scaffold constrained NWflash Tauri workspace`，仅包含新目录和锁文件。

### Task 3: 复刻窗口、设计令牌和应用外壳

**Files:**
- Create: `src/Nwflash.Desktop/src/app/App.tsx`、`window-state.ts`、`ipc-events.ts`
- Create: `src/Nwflash.Desktop/src/components/AppShell.tsx`、`Sidebar.tsx`、`DeviceStatusPanel.tsx`、`OperationProgressPanel.tsx`、`ModalLayer.tsx`
- Create: `src/Nwflash.Desktop/src/styles/tokens.css`、`reset.css`、`shell.css`、`components.css`
- Create: `src/Nwflash.Desktop/src/pages/{Overview,QuickFlash,Mirror,FileManager,LineFlash,FirmwareExtract,SafeFlash,Root,Online,Software,OperationLog}Page.tsx`
- Test: `src/Nwflash.Desktop/src/components/AppShell.test.tsx`、`src/Nwflash.Desktop/src/app/window-state.test.ts`

**Consumes:** Task 1 的截图和状态矩阵；`MainWindow.xaml`、`LoginWindow.xaml`、`App.xaml`。

**Produces:** 不调用真实设备的主窗 Shell；可切换 11 个占位页面，准确保留导航分组、账号栏、时钟、窗口控制、统一状态面板和模态层。

- [ ] **Step 1: 写失败前端测试**：覆盖 11 个导航项的文案、顺序和目标页；覆盖空闲时“无进行中的操作”；覆盖快速刷写、可视刷写、线刷、固件提取和通用设备操作的进度显示优先级；覆盖登出在 busy 状态下禁用。
- [ ] **Step 2: 运行测试确认失败**：运行 `npm run test --prefix src/Nwflash.Desktop -- AppShell window-state`；预期为测试文件/组件不存在。
- [ ] **Step 3: 实施视觉外壳**：从 WPF `App.xaml` 和 `MainWindow.xaml` 提取精确颜色、间距、字号、圆角、边框和阴影到 CSS token；实现自定义无边框顶栏及最小化/最大化/关闭；使用页面枚举切换而非 Web URL 路由；所有页面初期只呈现 Task 1 的静态基线状态，不添加业务逻辑或临时模拟授权。
- [ ] **Step 4: 运行测试与截图对比**：重复 Step 2 测试，启动 Tauri 开发版，按 Task 1 同名状态捕获 Shell 截图；预期导航、右侧状态区和空闲布局与基线一致，没有默认组件库主题。
- [ ] **Step 5: 提交**：提交 `feat(ui): recreate NWflash application shell and design tokens`。

### Task 4: 迁移领域模型、纯策略与错误分类

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/src/{lib,app_page,device,operation,partition,quick_flash,safe_flash,firmware,download,log,error}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-domain/tests/{partition_policy,safe_flash_slots,quick_flash_plan,firmware_models,operation_log}.rs`
- Read only: `src/VivoKsu.App/Models/**`、`PartitionRiskPolicy.cs`、`SafeFlashSlotPlanner.cs`、`PartitionExecutionPlanBuilder.cs`
- Test mapping: `PartitionRiskPolicyTests.cs`、`SafeFlashSlotPlannerTests.cs`、`PartitionExecutionPlanBuilderTests.cs`、`QuickFlashServiceTests.cs`、`OperationLogEntryTests.cs`

**Consumes:** Task 2 workspace。

**Produces:** Serde DTO、纯模型和策略；后续 crate 只能使用这些 Rust 类型，不得重新定义同含义的 UI 类型。

- [ ] **Step 1: 写失败 Rust 测试**：按旧测试名称迁移以下行为：高风险/mounted 分区拒绝规则、当前/对槽/双槽目标计算、快速刷写批量计划、`preloader*`/`lk` 过滤、日志等级中文标签、设备连接状态与大小格式化。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-domain`；预期为测试引用的模型和策略不存在。
- [ ] **Step 3: 实施纯领域层**：一一迁移 `Models/` 的值对象和枚举；用无 IO 的函数实现风险判断、槽位和刷写计划；错误枚举必须区分用户取消、设备不可用、授权拒绝、远端 API、外部工具、文件格式和内部错误。不要在 domain crate 引入 Tauri、Tokio、HTTP 或 Windows API。
- [ ] **Step 4: 运行测试确认通过**：运行 `cargo test -p nwflash-domain`；预期为本任务迁入的纯策略用例全部通过。
- [ ] **Step 5: 提交**：提交 `feat(core): port NWflash domain models and safety policies`。

### Task 5: 实现 Cloudflare 客户端、登录、会话和版本契约

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/{api_client,auth,version_client}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/{api_contract,auth_contract,version_contract}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/{auth,version}.rs`
- Modify: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/lib.rs`
- Test mapping: `OtaApiClientTests.cs`、`AppVersionControlTests.cs`、`HeartbeatServiceTests.cs`

**Consumes:** Task 4 DTO/error 分类，Task 1 API 用例矩阵。

**Produces:** `CloudflareClient` 的登录、当前用户、ROM、heartbeat、online、operation authorization、usage log 和版本检查接口；前端只获得无 token 的 session DTO。

- [ ] **Step 1: 写失败 HTTP 契约测试**：使用本地 mock server 重现 Task 1 中每个请求/响应；分别断言 `Authorization: Bearer`、`X-Nwflash-Version`、JSON snake_case、URL 编码，以及 401/402/403/404/426/429 到领域错误/中文显示码的映射。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-infrastructure --test api_contract --test auth_contract --test version_contract`；预期为 client 模块和 command 不存在。
- [ ] **Step 3: 实施 Rust-only 网络层**：将 C# `LoginService`、`OtaApiClient`、`AppVersionService` 的行为迁移为 Reqwest client；token 只保存在 Rust 应用状态，不写 localStorage、IndexedDB、配置文件或 React store；创建 `auth_login`、`auth_logout`、`version_check` command，输入只允许账号/密码等最小 DTO。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；再运行 Tauri 开发版，确认未登录时只有登录窗可见，失败时显示已有中文语义而不泄露 HTTP 响应体。
- [ ] **Step 5: 提交**：提交 `feat(auth): add Rust Cloudflare client and session gate`。

### Task 6: 迁移操作协调器、日志、授权门禁与退出收尾

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/{operation_coordinator,operation_context,usage_reporter,session_lifecycle}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/operation_log.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/{operation,session}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/{operation_coordinator,session_lifecycle}.rs`
- Modify: `src/Nwflash.Desktop/src/pages/OperationLogPage.tsx`
- Create: `src/Nwflash.Desktop/src/pages/OperationLogPage.test.tsx`
- Test mapping: `OperationCoordinatorTests.cs`、`OperationLogServiceTests.cs`、`UsageLogUploaderTests.cs`、`HeartbeatServiceTests.cs`

**Consumes:** Task 4 errors/models、Task 5 `CloudflareClient`。

**Produces:** `OperationCoordinator::run`、`cancel_current`、`OperationSnapshot` 事件、内存 500 条日志/磁盘日志、30 秒使用日志批量上传、优雅登出/强退/426 退出流程。

- [ ] **Step 1: 写失败测试**：分别覆盖授权被拒绝时任务零副作用、并发任务被拒、取消传播、成功/失败/取消使用日志、进度不写每条日志、日志只保留最新 500 条、心跳 transient failure 恢复、force exit 等待 busy 操作结束、426 触发更新窗口和 goodbye。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-application --test operation_coordinator --test session_lifecycle`；预期为协调器和生命周期实现不存在。
- [ ] **Step 3: 实施状态机与事件桥**：将 C# `OperationCoordinator`、`HeartbeatService`、`UsageLogUploader`、`OperationLogService` 的同步语义迁入 Rust；所有进度经单一 `operation:snapshot` 事件在最多 100ms 一次的频率发送，完成/取消/失败状态不得被节流丢弃；把最新 500 条日志事件接到 `OperationLogPage`；为 Rust panic、Tauri 未处理错误和进程退出清理写入 `%LOCALAPPDATA%\\Nwflash\\crash.log`/操作日志；服务端强退先取消当前操作并等待 coordinator idle，再关闭窗口/进程。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；在 Tauri 前端订阅 `operation:snapshot`，确认 Task 3 右上进度区和登出禁用态实时更新。
- [ ] **Step 5: 提交**：提交 `feat(core): port operation gate, audit log, and session lifecycle`。

### Task 7: 迁移路径、偏好、外部资源和下载完整性基础设施

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/{paths,preferences,remote_assets,resource_downloader,payload_provisioner,scrcpy_provisioner,root_resources}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/{paths,preferences,remote_assets,resource_downloader}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/resources.rs`
- Test mapping: `ExternalResourceLocationsTests.cs`、`ToolPathPreferencesTests.cs`、`RemoteAssetDownloaderTests.cs`、`PayloadDumperProvisionerTests.cs`、`ScrcpyProvisioningServiceTests.cs`、`OfficialKernelSuResourceTests.cs`

**Consumes:** Task 4 errors、Task 6 operation events/logging。

**Produces:** 机器级 `C:\\nwflash` 优先/用户目录回退、原子 staging、SHA-256、镜像 failover、资源就绪状态和可取消的资源下载事件。

- [ ] **Step 1: 写失败测试**：覆盖无法写 `C:\\nwflash` 时回退、scrcpy/manager APK/payload_dumper 的就绪检查、篡改哈希拒绝、直连失败后镜像顺序、文件长度/哈希检查、staging 失败不污染最终文件、取消不产生半成品。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-infrastructure --test paths --test preferences --test remote_assets --test resource_downloader`；预期为模块不存在。
- [ ] **Step 3: 实施资源基础设施**：从 `ExternalResourceLocations`、`ToolPathPreferences`、`RemoteAssetCatalog`、`RemoteAssetDownloader` 和三个 provisioner 逐条迁移；保留所有现有 SHA-256、发布资源名、镜像顺序和手动下载链接语义；任意下载过程经 coordinator 记录且只发送节流的 `resource:progress` DTO。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；用本地 mock 文件服务器验证每种成功/失败/取消路径，确认 token 和外部 URL 不进入 React 状态持久化。
- [ ] **Step 5: 提交**：提交 `feat(resources): port verified external resource provisioning`。

### Task 8: 迁移 Windows 子进程、ADB/Fastboot 与传输进度

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/src/{process_runner,process_io,platform_tools,adb,fastboot,adb_root_transfer,driver}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-windows/tests/{process_runner,fastboot,adb_root_transfer,driver}.rs`
- Test mapping: `SystemProcessRunnerTests.cs`、`FastbootPartitionTransportTests.cs`、`FastbootPartitionServiceTests.cs`、`AdbRootTransferRunnerTests.cs`、`VivoDriverDetectorTests.cs`、`VivoDriverInstallerTests.cs`

**Consumes:** Task 4 transport/operation types、Task 6 cancellation/logging、Task 7 resolved paths。

**Produces:** 受测的 `AdbClient`、`FastbootClient`、`ProcessRunner`、`AdbRootTransfer`、`DriverService`；所有外部调用只由 Rust 进行。

- [ ] **Step 1: 写失败测试**：用录制 runner 覆盖 adb/fastboot 精确参数、环境变量 `ADB` 指向内置 adb、serial 传递、stderr 合并、进程树取消、fastboot `getvar`/`flash`/`erase`/`reboot`/`set_active`、无进展超时在 IO 变化时续期、驱动已安装/安装失败判断。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-windows`；预期为 native client 与录制 runner 不存在。
- [ ] **Step 3: 实施 Windows 适配层**：用 Rust Windows API 读取子进程 IO 计数，移植 `FastbootCliRunner` 的进度和无进展超时；不移植旧 .NET/fastboot-rs DLL 机制；用内置 `platform-tools/adb.exe` 和 `fastboot.exe` 为唯一设备二进制；路径和参数均在 Rust 验证并使用参数数组启动。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；在无设备环境做 `adb version`/`fastboot --version` 只读烟测，确认任何失败经过领域错误显示而不是 panic。
- [ ] **Step 5: 提交**：提交 `feat(windows): port managed adb fastboot and process progress layer`。

### Task 9: 迁移设备会话、监视和概览操作

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/{device_session,device_monitor,device_info,overview}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/{device_session,device_monitor,device_info,overview}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/device.rs`
- Modify: `src/Nwflash.Desktop/src/components/DeviceStatusPanel.tsx`、`src/pages/OverviewPage.tsx`
- Test mapping: `DeviceSessionServiceTests.cs`、`DeviceMonitorServiceTests.cs`、`DeviceInfoServiceTests.cs`、`OverviewViewModelTests.cs`、`FastbootRsDeviceParserTests.cs`

**Consumes:** Task 6 coordinator、Task 8 clients。

**Produces:** `device_refresh`、`device_reboot_system`、`device_reboot_bootloader`、`device_reboot_fastboot` command 和 `device:snapshot` 事件；主 UI 使用真实设备状态。

- [ ] **Step 1: 写失败测试**：覆盖 ADB/Fastboot/unauthorized/offline 解析、手动刷新始终广播、心跳仅身份变化广播、断开两次才生效、连续三次自动失败降级、busy 时跳过轮询、操作结束补偿刷新、概览三种重启目标。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-application --test device_session --test device_monitor --test device_info --test overview`；预期为实现不存在。
- [ ] **Step 3: 实施设备状态机**：按 `DeviceSessionService` 与 `DeviceMonitorService` 迁移 3 秒轮询、防抖和补偿逻辑；将更新投射为 `device:snapshot`，绝不在自动刷新路径读取分区表；Overview React 页只 invoke 明确的重启命令。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；启动 UI，在无设备/ADB/fastboot 三种录制状态下验证状态卡、刷新按钮和概览提示。
- [ ] **Step 5: 提交**：提交 `feat(device): port session monitoring and overview actions`。

### Task 10: 接入登录窗口、在线状态、软件页和资源下载窗

**Files:**
- Create: `src/Nwflash.Desktop/src/pages/{Login,Online,Software,ResourceDownload}Page.tsx`
- Create: `src/Nwflash.Desktop/src/components/{DriverReminder,UpdateRequired}Modal.tsx`
- Create: `src/Nwflash.Desktop/src/features/{auth,online,resources,software,driver,version}.ts`
- Create: `src/Nwflash.Desktop/src/pages/{Login,Online,Software,ResourceDownload}Page.test.tsx`
- Create: `src/Nwflash.Desktop/src/components/{DriverReminder,UpdateRequired}Modal.test.tsx`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/{online,software,driver}.rs`
- Test mapping: `OnlineViewModelTests.cs`、`SoftwareViewModelTests.cs`、`ResourceDownloadViewModelTests.cs`、`MainViewModelTests.cs`

**Consumes:** Task 3 UI、Task 5 auth/version、Task 6 heartbeat、Task 7 resource provisioning、Task 8 driver service、Task 9 device snapshots。

**Produces:** 实际登录门禁、每秒显示时长/时钟、每 5 秒在线刷新、组件下载选择与取消、驱动提醒/重装、强制更新窗和优雅登出回登录窗。

- [ ] **Step 1: 写失败测试**：覆盖登录失败不显示主窗、登录成功显示账号、在线列表 self 标记和时长、网络失败显示旧数据状态、资源默认全选/部分选中文案、跳过不下载、取消停止队列、未安装驱动的提醒/取消/重装、426 更新窗只允许打开下载链接或退出、busy 时登出禁用、登出后清空前端敏感状态。
- [ ] **Step 2: 运行测试确认失败**：运行 `npm run test --prefix src/Nwflash.Desktop -- Login Online Software ResourceDownload`；预期为页面/IPC client 不存在。
- [ ] **Step 3: 实施前端接线**：仅经 Task 5/6/7/8/9 的 command 和事件使用真实数据；登录窗单独显示，成功后显示主窗；资源窗逐项显示已安装/下载中/成功/失败；驱动窗只调用受限 `driver_reinstall` command；更新窗只调用受限外链打开 command；软件页仅显示 Rust 传来的路径和就绪 DTO，不读取文件系统。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；在 mock Cloudflare 环境完成登录、在线、登出、资源取消和 426 更新路径冒烟。
- [ ] **Step 5: 提交**：提交 `feat(ui): connect auth online software and resource workflows`。

### Task 11: 迁移投屏和文件管理

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/{mirror,file_manager}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/{mirror,file_manager}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/{mirror,files}.rs`
- Modify: `src/Nwflash.Desktop/src/pages/{Mirror,FileManager}Page.tsx`
- Create: `src/Nwflash.Desktop/src/pages/{Mirror,FileManager}Page.test.tsx`
- Test mapping: `MirrorServiceTests.cs`、`FileManagerViewModelTests.cs`、`AdbFileServiceTests.cs`、`ScrcpyToolLocatorTests.cs`

**Consumes:** Task 6 coordinator、Task 7 paths/provisioner、Task 8 adb transfer、Task 9 session。

**Produces:** 安全的投屏启动/停止/自动投屏，及本地/设备文件浏览、上传、保存下载、删除、APK 安装命令。

- [ ] **Step 1: 写失败测试**：覆盖 ADB 未连接时禁用、scrcpy 缺失不启动、ADB 环境变量、主动停止不自动重启、设备变化时刷新文件状态、远端根目录上级禁用、删除确认捕获对象、保存对话框取消不下载、用户选择的精确目标路径、上传/下载走 coordinator。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-application --test mirror --test file_manager` 和 `npm run test --prefix src/Nwflash.Desktop -- Mirror FileManager`；预期为 use case 和页面测试失败。
- [ ] **Step 3: 实施受控文件与镜像路径**：通过受限 Tauri dialog capability 取得用户选中文件/目录，随后将绝对路径传入 Rust 再做存在性、类型、目标目录和设备连接验证；启动 scrcpy 时只由 Rust 设置 `ADB` 环境变量；删除/安装/上传/下载均需 coordinator 和明确确认状态。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；使用假 adb runner 验证调用参数，使用手工文件夹验证保存/取消不泄漏路径到日志。
- [ ] **Step 5: 提交**：提交 `feat(device): port screen mirror and file manager workflows`。

### Task 12: 迁移快速刷写和分区工作区

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/{quick_flash,partition_workspace,partition_execution}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/{quick_flash,partition_workspace,partition_execution}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/{quick_flash,partitions}.rs`
- Modify: `src/Nwflash.Desktop/src/pages/{QuickFlash,LineFlash}Page.tsx`
- Create: `src/Nwflash.Desktop/src/pages/{QuickFlash,LineFlash}Page.test.tsx`
- Test mapping: `QuickFlashServiceTests.cs`、`QuickFlashViewModelTests.cs`、`PartitionWorkspaceViewModelTests.cs`、`PartitionExecutionServiceTests.cs`、`FastbootPartitionTableParserTests.cs`

**Consumes:** Task 4 plans/risk policy、Task 6 coordinator、Task 8 Fastboot/ADB root、Task 9 session。

**Produces:** 安全快速刷写、分区表显式读取、备份/写入/擦除、行级与总进度、确认窗和跨页面镜像映射。

- [ ] **Step 1: 写失败测试**：覆盖未连接/错误模式拒绝、expected serial 检查、单预设只刷对应分区、双槽逻辑、确认前零副作用、取消、分区表不会被设备监视自动读取、ADB Root/Fastboot 自动选择、mounted/high-risk 分区拒绝、备份文件长度校验、首个失败停止。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-application --test quick_flash --test partition_workspace --test partition_execution`；预期为 use case 不存在。
- [ ] **Step 3: 实施刷写编排**：使用 Task 4 计划生成器和风险策略；所有 flash/erase/backup 都经 Task 6 协调器；分区表只响应 `partitions_refresh` 明确 command；将当前分区、行级进度、总进度、速度和确认摘要以 DTO 发送给前端；从固件/Root 页面来的镜像只能经受测的 `quick_flash_prepare_image` 状态入口进入。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2，并运行 `npm run test --prefix src/Nwflash.Desktop -- QuickFlash LineFlash`；预期为确认窗、禁用态和右上进度区全部符合 Task 1 基线。
- [ ] **Step 5: 提交**：提交 `feat(flash): port quick flash and partition workspace`。

### Task 13: 迁移固件包检查、payload 与 VIVO 固件提取

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/{firmware_package,payload_dumper,vivo_firmware,firmware_extract}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/firmware_extract.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/tests/{firmware_package,payload_dumper,vivo_firmware,firmware_extract}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/tests/firmware_extract.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/firmware.rs`
- Modify: `src/Nwflash.Desktop/src/pages/{FirmwareExtract,LineFlash}Page.tsx`
- Test mapping: `FirmwarePackageInspectorTests.cs`、`FirmwarePackageExtractionServiceTests.cs`、`PayloadDumperRunnerTests.cs`、`FirmwarePartitionExtractorTests.cs`、`VivoFirmwareExtractorTests.cs`、`FirmwareExtractViewModelTests.cs`、`LineFlashViewModelTests.cs`

**Consumes:** Task 6 coordinator、Task 7 payload provisioner、Task 8 process IO progress。

**Produces:** 本地/远程 firmware source 的格式判定、分区列出/选择/提取、连续进度、取消和“刷入此镜像”向快速刷写的受控映射；VIVO 固件包中的 managed image 检查/提取也在 LineFlash 页保留。

- [ ] **Step 1: 写失败测试**：覆盖 zip/payload/目录/Vivo gzip 魔数分流、payload 只读所需 Range、gzip tar 流式解压、base-256 长度、managed image 过滤、LineFlash 固件包只列出 boot/init_boot/vendor_boot/lk、local gzip 不被误删、提取产物路径与大小、payload 进程写字节进度、取消和损坏包中文错误。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-infrastructure --test firmware_package --test payload_dumper --test vivo_firmware --test firmware_extract` 和 `cargo test -p nwflash-application --test firmware_extract`；预期为模块不存在。
- [ ] **Step 3: 实施提取链路**：保留 `payload_dumper.exe` 作为受校验的外部工具；Rust 负责其获取、命令、进程监视和输出校验；按 C# `VivoFirmwareExtractor` 的格式规则迁移 gzip/tar/zstd 处理；不要下载完整远程 payload；当前分区/百分比/速度/耗时事件继续节流到 100ms。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；在 React 页验证分区选择、输出目录、停止按钮、完成后映射快速刷写且不自动刷入。
- [ ] **Step 5: 提交**：提交 `feat(firmware): port payload and VIVO extraction workflows`。

### Task 14: 迁移 VIVO 线刷（安全刷写）

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/ota_download.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/embedded_assets.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/safe_flash.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-{infrastructure,application}/tests/{ota_download,safe_flash}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/safe_flash.rs`
- Modify: `src/Nwflash.Desktop/src/pages/SafeFlashPage.tsx`
- Create: `src/Nwflash.Desktop/src/pages/SafeFlashPage.test.tsx`
- Test mapping: `OtaDownloadServiceTests.cs`、`SafeFlashViewModelTests.cs`、`FirmwarePartitionExtractorTests.cs`、`SafeFlashSlotPlannerTests.cs`、`EmbeddedWipeDataTests.cs`

**Consumes:** Task 4 slot/firmware rules、Task 5 ROM client、Task 6 coordinator、Task 8 fastboot、Task 13 extraction。

**Produces:** 线上/本地 ZIP/payload/已提取目录的线刷 use case；槽位、清除数据、确认、进度、失败保留 staging 和重启规则与 WPF 等价。

- [ ] **Step 1: 写失败测试**：覆盖 Range server 全范围/单连接选择、256MB 内存缓冲上限、磁盘不足、进度节流和最终 100%；覆盖 `preloader*`/`lk` 跳过、fastbootd 等待、设备缺失分区跳过、对槽/双槽/无 A/B 降级、`misc` 最后清除数据、失败不删除可恢复 staging、成功清理临时下载。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-infrastructure --test ota_download`、`cargo test -p nwflash-application --test safe_flash` 和 `npm run test --prefix src/Nwflash.Desktop -- SafeFlash`；预期为实现和页面不存在。
- [ ] **Step 3: 实施线刷状态机**：操作前使用 Task 5 获取 ROM 并进入 Task 6 coordinator；保留 WPF 的下载→提取→fastbootd→分区预检→逐个刷写→重启编排，绝不在 UI 内拼接分区列表；只在勾选清除数据时从嵌入资源安全落盘 `wipe-data.img`，并在普通分区后最后写入 `misc`；确认内容、选项默认值、不可用分区文案、右上主进度和停止按钮与基线一致。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；用录制 fastboot runner 验证无设备/取消/首分区失败/成功清理，使用只读模拟服务验证线上 ROM 不泄露到前端持久化状态。
- [ ] **Step 5: 提交**：提交 `feat(safe-flash): port VIVO OTA download and flash workflow`。

### Task 15: 迁移 Vivo ROOT 与 Root 资源处理

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-infrastructure/src/{root_resources,root_patch,vendor_boot}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-application/src/root.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-{infrastructure,application}/tests/{root_resources,root_patch,vendor_boot,root}.rs`
- Create: `src/Nwflash.Desktop/src-tauri/crates/nwflash-tauri/src/commands/root.rs`
- Modify: `src/Nwflash.Desktop/src/pages/RootPage.tsx`
- Create: `src/Nwflash.Desktop/src/pages/RootPage.test.tsx`
- Test mapping: `VivoRootResourceServiceTests.cs`、`OfficialKernelSuResourceTests.cs`、`RootPatchArtifactServiceTests.cs`、`VivoVendorBootProcessorTests.cs`、`VivoKsuDevicePatchServiceTests.cs`、`RootViewModelTests.cs`

**Consumes:** Task 6 coordinator、Task 7 verified resources、Task 8 adb/fastboot、Task 12 quick flash continuation。

**Produces:** KSU/KernelSU 资源验证、init_boot/vendor_boot 补丁、KMI 解析、自动 Root、管理器安装与“移交快速刷写”流程。

- [ ] **Step 1: 写失败测试**：覆盖 manager APK SHA-256 与 AndroidManifest 身份、篡改拒绝、magiskboot/`libksud` 提取、已有修补输出覆盖行为、自动 KMI/手动 KMI、设备串号/fastbootd 前置条件、取消、补丁产物只能映射到正确快速刷写预设。
- [ ] **Step 2: 运行测试确认失败**：运行 `cargo test -p nwflash-infrastructure --test root_resources --test root_patch --test vendor_boot`、`cargo test -p nwflash-application --test root` 和 `npm run test --prefix src/Nwflash.Desktop -- Root`；预期为模块和页面不存在。
- [ ] **Step 3: 实施 Root 链路**：保持 C# `VivoRootResourceService`、`VivoVendorBootProcessor`、`VivoKsuDevicePatchService` 的校验顺序和外部工具边界；所有命令执行经 coordinator；React 只呈现所选文件元数据、预检结果、产物元数据及显式“转到快速刷写”意图。
- [ ] **Step 4: 运行测试确认通过**：重复 Step 2；确认没有 APK 路径、哈希、KMI 或错误堆栈以外的敏感调试内容写入浏览器存储。
- [ ] **Step 5: 提交**：提交 `feat(root): port verified Vivo ROOT workflows`。

### Task 16: 完成页面保真、跨页面状态和自动化 UI 验收

**Files:**
- Modify: `src/Nwflash.Desktop/src/components/**`、`src/Nwflash.Desktop/src/pages/**`、`src/Nwflash.Desktop/src/styles/**`
- Create: `src/Nwflash.Desktop/src/test/visual-state-fixtures.ts`
- Create: `src/Nwflash.Desktop/e2e-tests/{package.json,package-lock.json,wdio.conf.ts}`
- Create: `src/Nwflash.Desktop/e2e-tests/specs/{navigation,dialogs,progress,visual-baseline}.ts`
- Modify: `docs/migration-baselines/2026-08-15-wpf-behavior-baseline.md`

**Consumes:** Task 3 和 Tasks 9–15 的真实 DTO/事件；Task 1 截图。

**Produces:** 全部 11 页和模态窗在空闲/加载/错误/运行状态下的可重复 UI 验收；不改变已验证的业务规则。

- [ ] **Step 1: 写失败 UI 测试**：覆盖所有导航、登录/关闭/登出、更新要求、驱动提醒、资源下载、分区删除/刷写确认、快速刷写确认、线刷确认、取消/停止、页面间产物转移与 busy 禁用；每个测试断言中文文案和关键 accessibility label。
- [ ] **Step 2: 运行测试确认失败**：建立采用 `@wdio/tauri-service` 的 WebdriverIO 配置（embedded provider），运行 `npm --prefix src/Nwflash.Desktop/e2e-tests test` 及 `npm --prefix src/Nwflash.Desktop run test`；预期为视觉 fixture/端到端测试不存在。
- [ ] **Step 3: 逐页修正而非重设计**：按 Task 1 截图逐页对比，修复 CSS token、间距、滚动、溢出、焦点、禁用态、进度条、日志自动滚动和模态层级；保持所有已有文字、导航顺序和操作名称，不引入新页面或新功能。
- [ ] **Step 4: 运行测试确认通过**：执行 `npm --prefix src/Nwflash.Desktop run test` 与 `npm --prefix src/Nwflash.Desktop/e2e-tests test`；WebdriverIO 必须使用 `@wdio/tauri-service` 的 embedded provider，允许 mock Tauri command 并捕获前后端日志；生成同名新截图并在基线文档逐项标记“等价”“允许的框架差异”或“阻断差异”。只有“等价/允许”才能通过。
- [ ] **Step 5: 提交**：提交 `test(ui): certify Tauri visual and interaction parity`。

### Task 17: 建立桌面端的全量测试、故障注入与真机验收矩阵

**Files:**
- Create: `src/Nwflash.Desktop/src-tauri/tests/e2e/{api,operation,device_process,firmware}.rs`
- Create: `docs/migration-baselines/tauri-test-mapping.md`
- Create: `docs/migration-baselines/device-acceptance-matrix.md`
- Modify: `src/Nwflash.Desktop/README.md`

**Consumes:** Tasks 4–16 的单元、集成和 UI 测试。

**Produces:** C# 测试到 Rust/前端测试的一对一映射，安全的录制/模拟验收和经批准的真机矩阵。

- [ ] **Step 1: 写失败映射检查**：建立清单，要求每个 `tests/VivoKsu.App.Tests/*Tests.cs` 至少映射到一个 Rust、前端或明确的“被合并测试”条目；缺失条目使检查失败。
- [ ] **Step 2: 运行检查确认失败**：运行映射检查与 `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace`；预期为映射缺失或功能测试尚未完整通过。
- [ ] **Step 3: 补齐测试与故障注入**：为未映射的 C# 行为补 Rust 或前端测试；mock Cloudflare、GitHub、下载服务器、adb/fastboot/payload/scrcpy 进程；写入真机矩阵，至少区分只读设备检测、取消、失败收尾、资源下载和授权的安全测试，以及需要专用可恢复设备的刷写/Root 测试。
- [ ] **Step 4: 运行全量验证确认通过**：运行 `cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace`、`npm run test --prefix src/Nwflash.Desktop`、生产构建和 UI 自动化；预期为 0 失败，映射清单无缺项。
- [ ] **Step 5: 提交**：提交 `test: complete Rust migration parity coverage`。

### Task 18: 实现 Tauri 资源打包、NSIS 与可重复发布脚本

**Files:**
- Create: `scripts/Publish-TauriRelease.ps1`
- Create: `scripts/Verify-TauriRelease.ps1`
- Create: `src/Nwflash.Desktop/src-tauri/resources/README.md`
- Modify: `src/Nwflash.Desktop/src-tauri/tauri.conf.json`
- Modify: `README.md`、`docs/architecture.md`
- Test: `scripts/verify-tauri-release.ps1`（或 PowerShell Pester/等价测试）

**Consumes:** Tasks 2、7、16、17 的可构建工程和资源策略。

**Produces:** 无 .NET Runtime 的 NSIS Windows x64 安装包、资源清单、SHA-256 清单、WebView2 embedded bootstrapper、安装/卸载烟测。

- [ ] **Step 1: 写失败发布脚本测试**：在临时发布目录断言 Tauri release EXE、前端资源、platform-tools、drivers、root-tools 与需要随包的文件被包含；断言 scrcpy/APK/payload_dumper 不被错误地强制随包；断言安装器模式为 per-user 且使用 `embedBootstrapper`。
- [ ] **Step 2: 运行测试确认失败**：运行 `powershell -ExecutionPolicy Bypass -File scripts/Verify-TauriRelease.ps1 -DryRun`；预期为脚本或发布布局不存在。
- [ ] **Step 3: 实施发布链**：以 `tauri build -- --no-bundle` 生成待保护 EXE，再由独立脚本执行后续保护/签名步骤；配置 NSIS、品牌图标、中文安装器文案、WebView2 bootstrapper、Windows x64 资源复制和 SHA-256 清单；迁移 README/architecture 的构建、安装、外置资源与故障排查说明，不改 Cloudflare 部署文档。
- [ ] **Step 4: 运行发布验证确认通过**：重复 Step 2，再运行非保护 release 构建和安装/卸载烟测；预期为安装后不需要 .NET Runtime，登录窗可出现，缺少外置资源只显示可恢复的下载状态。
- [ ] **Step 5: 提交**：提交 `build: add Tauri Windows release and installer workflow`。

### Task 19: 接入 VMProtect、Authenticode 签名、保护后验证与切换门禁

**Files:**
- Create: `packaging/vmprotect/nwflash.vmp`（不含许可证、证书或绝对机器路径）
- Create: `scripts/Protect-NwflashRelease.ps1`
- Create: `scripts/Sign-NwflashRelease.ps1`
- Create: `scripts/Verify-ProtectedRelease.ps1`
- Modify: `scripts/Publish-TauriRelease.ps1`
- Create: `docs/release/tauri-vmp-signing-runbook.md`
- Modify: `README.md`、`docs/architecture.md`

**Consumes:** Task 18 的未保护 EXE/NSIS 包与 Task 17 的完整验收命令。

**Produces:** 可重复的 build → 测试 → VMP → 保护后测试 → EXE 签名 → NSIS 打包 → 安装包签名 → 验签/哈希流程；WPF 到 Tauri 的发布切换检查表。

- [ ] **Step 1: 写失败保护流水线测试**：在临时目录创建假 VMP/签名命令，断言发布脚本的调用顺序严格为：未保护测试完成、VMP 处理最终 EXE、保护后烟测、签名 EXE、生成 NSIS、签名 NSIS、`Get-AuthenticodeSignature` 验证、SHA-256 清单；任一步失败必须停止且不发布安装器。
- [ ] **Step 2: 运行测试确认失败**：运行 `powershell -ExecutionPolicy Bypass -File scripts/Verify-ProtectedRelease.ps1 -DryRun`；预期为保护/签名脚本和 `.vmp` 配置不存在。
- [ ] **Step 3: 实施选择性保护和签名**：将 VMP 路径、签名工具、证书 thumbprint 作为受控 CI/发布环境变量读取；仅标记构建身份/完整性检查、授权响应与当前操作绑定、少量本地策略分派等小型 Rust 函数；排除 Tauri 入口、WebView、Tokio、事件桥、外部进程控制、长下载/解包循环和第三方库；不启用拒绝虚拟机运行；任何保护失败仅产生可诊断日志和发布失败，不静默跳过。
- [ ] **Step 4: 运行保护后验证确认通过**：在干净 Windows 10/11 环境执行 `Verify-ProtectedRelease.ps1`、安装包签名验证、完整 Rust/前端测试、登录/API mock 烟测、资源下载窗、只读 adb/fastboot 检测和专用测试设备上的批准验收；预期为签名有效、功能无回归且无 `.NET` 依赖。
- [ ] **Step 5: 提交并执行切换门禁**：提交 `build: protect and sign NWflash Tauri releases`；依据 `docs/release/tauri-vmp-signing-runbook.md` 完成发布前检查。只有 Task 1 基线、Task 17 测试映射、Task 16 UI 对比和本任务保护后真机验收均通过时，才允许将 Tauri 安装包设为默认下载；旧 WPF 工程在此之后仍保留一个发布周期作为回退。

## 最终验收命令

实施 agent 在宣布迁移完成前必须保存以下命令的输出：

```powershell
dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug
npm --prefix src/Nwflash.Desktop ci
npm --prefix src/Nwflash.Desktop run test
npm --prefix src/Nwflash.Desktop run build
npm --prefix src/Nwflash.Desktop/e2e-tests ci
npm --prefix src/Nwflash.Desktop/e2e-tests test
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace
npm --prefix src/Nwflash.Desktop run tauri -- build -- --no-bundle
powershell -ExecutionPolicy Bypass -File scripts/Verify-TauriRelease.ps1
powershell -ExecutionPolicy Bypass -File scripts/Verify-ProtectedRelease.ps1
```

完成定义是：旧 WPF 基线测试保持通过；Tauri/Rust 全套测试、视觉验收和保护后烟测通过；`cloudflare/**` 无修改；EXE 和 NSIS 安装包签名/哈希可验证；经过批准的真机矩阵完成且不存在阻断级设备安全问题。
