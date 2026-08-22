# 奶蛙Flash Rust/Tauri 客户端架构

## 范围与事实来源

本文是 `src/Nwflash.Desktop/` 客户端源码的架构入口。描述以当前 Rust、Tauri 和 React 源码为准，不把旧 WPF 实现或未来整改计划当作已经可用的接口。

主要事实来源：

- `src-tauri/Cargo.toml`：Cargo workspace 成员和依赖方向。
- `src-tauri/crates/nwflash-tauri/src/lib.rs`：Tauri 应用状态、事件绑定和公开 handler 注册表。
- `src-tauri/crates/nwflash-tauri/src/commands/`：命令参数、运行时 capability 和 DTO 边界。
- `src-tauri/crates/nwflash-application/`、`nwflash-infrastructure/`、`nwflash-windows/`：用例编排、外部资源和 Windows 适配器。
- `src/`：React 页面、事件消费和原生文件对话框入口。

`cloudflare/**` 是独立后端契约目录，不属于本客户端 workspace，也不在本架构文档的修改范围内。

## Workspace 与依赖方向

```text
src/Nwflash.Desktop/
├─ src/                              # React 壳体、页面和 IPC DTO 类型
├─ src-tauri/
│  ├─ src/main.rs                    # 唯一 Tauri 二进制入口
│  ├─ build.rs                        # tauri-build 上下文生成
│  ├─ Cargo.toml                      # workspace 根
│  └─ crates/
│     ├─ nwflash-domain              # 领域模型、错误和封闭枚举
│     ├─ nwflash-windows             # Windows 进程、设备和驱动适配器
│     ├─ nwflash-infrastructure      # Cloudflare、下载、日志和资源 provisioner
│     ├─ nwflash-application         # 用例、协调器、设备会话和流程编排
│     └─ nwflash-tauri               # AppState、Tauri command/event 桥
└─ docs/rust-tauri-architecture.md   # 本文
```

依赖方向由 Cargo manifest 固定：`nwflash-domain` 不依赖项目内其他 crate；`nwflash-windows` 依赖 domain；`nwflash-infrastructure` 依赖 domain；`nwflash-application` 组合 domain、infrastructure 和 windows；`nwflash-tauri` 依赖四个 crate 并拥有 Tauri 类型。`src-tauri/src/main.rs` 只调用 `nwflash_tauri::run_app`，因此 command 注册集中在一个宿主中。

## 运行时与 IPC 边界

### 启动和状态

1. `main.rs` 调用 `run_app(tauri::generate_context!())`。
2. `run_app` 创建 `AppState`，绑定 operation、firmware progress、session 和 device 事件，并注册公开 handler。
3. React `App.tsx` 先执行版本门禁，再读取 `session_state` 和 `auth_validate_token`；有效会话启动 `session_start`，退出调用 `session_stop`/`auth_logout`。
4. Rust 通过 `operation:snapshot`、`firmware:progress`、`device:snapshot`、`session:*` 事件把路径安全的状态 DTO 推送给 React。

### 公开能力和 Rust 内部 capability

> 产品决策（2026-08-21）：当前业务流程已移除运行时镜像/工件/OTA 哈希门禁，以及跨步骤 serial 比较和设备 serial 绑定 capability。serial 仅保留为当前命令目标和界面显示；发行物与受控资源的完整性校验保留。详见仓库根目录 [product-decisions.md](../../../docs/product-decisions.md)。

公开 handler 的输入按 command 分别约束，常见输入包括封闭枚举、用户确认、路径或不透明 ID。UI 通常用原生 dialog 取得本地路径，但 IPC 的 `String` 参数本身不能证明 dialog provenance。以下数据不作为浏览器可提交的任意能力提供：

- `AppState.session_token` 中的 bearer token。`auth_login` 返回的 `AuthSessionDto` 只有 `username` 和 `name`；`AuthSessionPayload` 同样没有 token 字段，`session_state` 只报告 `has_token`。
- 浏览器不能提交原始 `PartitionExecutionPlan`、任意命令数组、任意程序名、任意 shell 文本或通用 ROM/固件解析入口；但公开 Quick Flash prepare API 当前会把 Rust 生成的 serial 和 `ProcessCommandDto` 程序/参数作为 `QuickFlashPlanDto` 预览返回，这是现有/遗留 API 暴露限制，不是执行输入。
- 用户输入的 HTTP(S) 固件 URL 是受限入口：`FirmwareExtractPage` 只把它提交给 `firmware_inspect_remote`/`firmware_extract_remote`，Rust 复核 scheme 与 host；提取还必须匹配已检查的 URL，并只接受该次检查生成的不透明分区 ID。服务端解析的 ROM/OTA URL 不属于这个用户输入边界，仍只留在 Rust runtime，并由受控设备信息和内存 token 驱动。浏览器不能提交任意程序、命令数组、shell 文本、未校验资源路径或任意远程分区 ID。
- `DeviceSnapshot`、TypeScript `DeviceSnapshotPayload` 和现有 `QuickFlashPlanDto` 预览可包含 serial；它不是公开执行输入，浏览器不能提交、选择或伪造 execution serial。服务端解析来源、staging、固件工件、prepared dual-slot、ROOT 和 Safe Flash 的受保护计划仍只通过安全摘要或 capability ID 暴露；通用 Quick Flash 镜像路径与远程固件输出目录是下述当前例外。

`quick_flash_prepare_commands`/`quick_flash_execute_commands` 是 Rust 内部 helper，但公开的 `quick_flash_prepare_boot_image`/`quick_flash_prepare_preset_image` 会返回上述命令预览。当前 `QuickFlashPage` 把原生 dialog 选出的 `imagePath`/封闭分区请求保存在 React 确认状态，随后提交给 `quick_flash_execute_preset_images`；Rust 在该次调用内重新检查镜像，并在构造命令前解析当前唯一 transport serial。该 serial 覆盖计划/预览中的历史值，并用于 flash、切槽和重启；流程不消费 Rust-owned confirmation capability。

### 单设备操作模型

产品每次启动只服务当前发现的一台设备。设备发现同时得到多个设备时返回 `MultipleDevices` 拒绝态；`DeviceRuntime` 保存最新 `DeviceSnapshot`。该快照的 serial 会投射给 React 显示，但浏览器没有执行 serial 参数或设备选择器。Rust 只在即将构造当前 ADB/Fastboot 命令时从 `DeviceRuntime` 派生目标；计划、预检、工件和 OTA 状态不以历史 serial 等值匹配作为消费或阶段推进门禁。

### 操作协调

耗时 command 通过 `OperationCoordinator` 获得互斥、授权、进度、日志和取消语义。Windows 适配器接收 `ProcessCommand` 的程序名、参数数组、工作目录和环境变量，由 Rust 组装后启动；取消或超时会终止进程树。前端不能改写这些字段。

## 设备与操作模型

设备探测由 `nwflash-windows::PlatformDeviceDiscovery` 执行固定的 ADB/Fastboot 发现命令，application 层的 `DeviceSession` 和 `DeviceMonitor` 负责快照、防抖和错误降级。Tauri 只把最新快照投影给 Overview 和受控 command。

通用预设 Quick Flash 使用 browser-held 路径/请求和单次 execution invocation 内的 Rust 复检，不使用不透明确认 capability。固件工件、prepared dual-slot、ROOT 和 Safe Flash 则使用私有 runtime 保存不透明工件、计划或 session，确认执行时重新解析或一次性消费；分区工作区也从 Rust 当前快照和私有映射重建执行计划。

本地输入/输出路径在正常 UI 交互中通常来自原生对话框，Rust 会按具体流程检查存在性、后缀、大小、归档成员或写入结果；不能把它概括为每个公开路径都已证明 dialog provenance。特别是 `firmware_extract_remote(output_directory: String)` 当前把字符串直接转为提取目录：Rust 仍验证 HTTP(S) URL、inspected-source 等值、不透明 selected ID、归档成员，并以 partial/大小校验/原子发布写入，但 command 层没有证明目录来源，这是后续 hardening 边界。

### ROOT 服务器 OTA 云提取

ROOT 页可以在已登录且连接 ADB 设备时调用 `root_ota_check`。命令从当前设备读取 PD/系统版本，以 Rust 内存中的 session token 请求 ROM 解析服务，并把返回的 OTA URL、名称、PD、版本和 session epoch 存入私有 `RootOtaRuntime`，不保存手机 serial 绑定。React 只收到“可用”及安全显示标签；独立的 `DeviceSnapshotPayload` serial 仅供界面显示，无设备、无会话或解析失败统一投影为不可用状态。

用户选择服务器来源后，`root_ota_extract_images` 不接收浏览器 serial：它只在构造当前 ADB 命令时从 `DeviceRuntime` 取得目标，不与私有 runtime 中的历史 serial 比较，也不在提取完成后做跨步骤 serial 复核。镜像以 session epoch 和不透明 ID 注册为 capability：

- `payload.bin` OTA 仅在探测到 payload 格式后准备受 SHA-256 校验的 `payload_dumper`，工具直接处理远程 URL 的 HTTP Range 请求；
- 直接镜像 ZIP 由 `remote_firmware::RangeHttpReader` 读取 ZIP 中央目录，并只解压 `init_boot`（优先）或 `boot`（回退）以及 `vendor_boot`，不下载完整 OTA；
- 产出的文件位于 Rust 所有的 staging，注册为不透明 ROOT 镜像 ID。`boot` 回退会随 ID 带上实际目标分区名，后续 Vivo KSU 修补和刷写使用该真实分区而不是硬编码 `init_boot`。

运行时替换或会话结束会清理已拥有的云提取 staging。ROOT OTA 的公开 DTO 不包含 OTA URL、serial、PD、版本或路径；网络、归档和文件系统原始错误同样在应用层归一化后才返回页面。

## 资源供应与完整性

`nwflash-infrastructure` 的 provisioner 负责校验内置工具和 ROOT 管理器资源，并用 staging 文件保护 ROM/OTA 等运行时内容，不把固定工具资源下载到本地缓存。

- **scrcpy**：发布包内置完整 `resources/scrcpy` 目录及 `scrcpy-files.sha256`；Rust 校验每个文件后使用，不接受用户自定义路径，也不回退到用户 `PATH`。
- **ROOT manager APK**：KSU 和 KernelSU APK 均在 bundle 中执行非空、SHA-256、ZIP 可读性和 `AndroidManifest.xml` 检查。
- **payload_dumper**：bundle executable 使用固定 SHA-256 校验；缺失或损坏时提示重新安装应用。
- **随包资源**：platform-tools、驱动、root-tools、scrcpy、ROOT APK 和 payload_dumper 都由 Tauri release resources 配置打包；React 不获得资源绝对路径。

## 构建、测试与发布

在仓库根目录执行：

```powershell
npm --prefix src/Nwflash.Desktop run test
npm --prefix src/Nwflash.Desktop run build
cargo test --manifest-path src/Nwflash.Desktop/src-tauri/Cargo.toml --workspace
npm --prefix src/Nwflash.Desktop run tauri -- build --no-bundle
```

`npm run build` 生成 Vite 前端 `dist/`；Tauri no-bundle 构建再由 Cargo 编译 Rust workspace 和桌面宿主，不创建 NSIS 安装器。正式发布脚本默认执行保护、签名、测试、安装验证和清单校验；未签名路径必须显式使用 `-DevelopmentUnsigned`，正式发布不能使用该开关。Windows release 资源边界见仓库级 [项目架构](../../../docs/project-architecture.md) 与 [README.md](../README.md)。

源码归档应包含 `src/`、`src-tauri/`、`e2e-tests/`、配置、测试、资源和本目录文档，但不包含 `node_modules/`、`dist/`、`src-tauri/target/`、运行日志、Tauri release staging 或 VMProtect 产物。

## 当前实现状态与外部验收边界

- **Task 4**：进程 stdout/stderr 在子进程运行期间由独立 reader 并发排空；正常完成会在构造输出前回收 reader，取消或超时会在终止并回收子进程后回收 reader。大输出与 reader 失败回归测试覆盖该边界。
- **运行时完整性边界**：ROOT 镜像/修补工件不再记录或重新计算运行时 SHA-256/fingerprint；路径、格式、大小、不透明 ID、session epoch 和 staging 所有权校验保留。发行物和受控资源的 SHA-256 校验不受影响。
- 真机刷写、驱动安装和最终签名/保护发布仍需按验收矩阵执行；本地 mock 测试不能替代设备验收。
