# 奶蛙Flash桌面端 Tauri/Rust 迁移设计规格

**日期：** 2026-08-15
**状态：** 已确认方案，待实施计划
**迁移对象：** 当前 `src/VivoKsu.App/` WPF 桌面端及 `src/VivoKsu.Bootstrapper/` 运行时引导器

## 1. 决策摘要

桌面端迁移到 **Tauri v2 + React + TypeScript + Vite + Rust**：React/CSS 复刻当前 WPF 的视觉和交互，Rust 负责所有设备、文件、网络、授权与刷写业务。最终 Windows 主程序使用 **VMProtect（VMP）** 进行选择性原生代码保护，再进行 Authenticode 签名和 NSIS 安装包签名。

不把“客户端无法被修改”作为安全前提。客户端可被逆向、前端资源可被读取、IPC 调用也应被视作不可信输入；因此 API token、上游 VOTA 凭据和最终授权继续由现有 Cloudflare 服务端持有和裁决。客户端篡改不得获得未授权的刷写、下载或 Root 操作能力。

## 2. 范围

### 2.1 迁移范围

| 源路径 | 迁移处置 |
| --- | --- |
| `src/VivoKsu.App/**` | 全部按功能迁移：WPF UI、模型、ViewModel、服务、内置资源引用。 |
| `src/VivoKsu.Bootstrapper/**` | 退役；Rust/Tauri 发布物不再依赖 .NET Desktop Runtime。 |
| `tests/VivoKsu.App.Tests/**` | 以测试行为为基线，迁移为 Rust 单元/集成测试与桌面验收用例。 |
| `scripts/Publish-Release.ps1` | 改为调用 Tauri/Rust 构建、VMP、签名、NSIS 打包与校验的发布脚本。 |
| `scripts/verify-*.ps1` | 保留其验收意图；按 Tauri 的自动化能力改写。 |
| `src/VivoKsu.App/{platform-tools,drivers,root-tools,apk,payload-tools,scrcpy}` | 保留为随包或按需下载的外部资源；不把 adb、fastboot、scrcpy、payload_dumper、APK、驱动或 magiskboot 改写为 Rust。 |

### 2.2 明确不迁移、不修改

`cloudflare/**` 整个目录均不迁移也不修改，包含 Worker、`cloudflare/web/` 后台、`cloudflare/user/` 用户门户和 `cloudflare/website/` 官网。桌面端只重新实现已有 API 的客户端调用，接口路径、请求字段、响应字段、错误码、认证方式和服务端业务语义保持兼容。

本迁移不改变品牌约定：用户可见名称为“奶蛙Flash”，技术标识使用 NWflash/NWF，既有域名和 `X-Nwflash-Version` 头保持不变。

## 3. 现状基线与不可回归行为

当前桌面端为 `.NET 8 + WPF + MVVM`，包含 118 个 C# 文件（约 13,739 行）、6 个 XAML 文件（2,718 行）和 50 个测试文件。主窗口包含 11 个功能页面，登录、驱动提醒、资源下载、强制更新等使用独立窗口或模态窗口。

迁移必须保留的核心行为如下：

1. **登录门禁与会话**：每次启动登录；登录后注入 token；心跳、在线列表、强制退出、426 强制更新和退出时 goodbye 均保持语义一致。
2. **单一操作闸门**：所有耗时且可能影响设备的操作串行、可取消、失败即停止；操作许可先经服务端检查；结果写操作日志并批量上传。
3. **设备监视规则**：3 秒心跳、身份变化/手动/操作完成才触发下游协调；分区表仅由用户显式读取，绝不在普通设备刷新中重读。
4. **刷写安全规则**：所有刷写经唯一 `fastboot.exe`；刷写进度依据进程 IO 采样；空闲无进展超时而不是固定总超时；首个失败分区停止；取消时终止进程树；关键路径验证设备串号、槽位、分区风险和镜像存在性。
5. **固件与 Root 行为**：payload/ZIP/Vivo gzip 格式分流、HTTP Range 按需下载、预留磁盘空间、临时目录清理规则、APK/工具 SHA-256 校验、vendor_boot 和 KernelSU 处理规则均按现有测试和源码复刻。
6. **界面行为**：11 个页面、左侧导航分组、底部账号/时钟/登出、右上角统一操作进度区、中文提示、确认窗、进度节流与跨页“刷入此镜像”跳转均保留。

现有 `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug --no-restore` 在迁移规划时以退出码 0 通过。Rust 实现的验收依据是这些测试表达的行为，不以逐行翻译 C# 或 XAML 为目标。

## 4. 目标架构

### 4.1 新目录布局

新桌面工程放入 `src/Nwflash.Desktop/`，不覆盖旧工程。旧 WPF 工程在 Rust 版本达到发布验收前保留，作为行为与视觉对照。

```text
src/Nwflash.Desktop/
├─ package.json / vite.config.*              # React/TypeScript 构建
├─ src/                                      # 前端 UI（仅展示、交互、状态投影）
│  ├─ app/                                   # 启动、路由、Tauri 事件订阅
│  ├─ components/                            # Shell、卡片、按钮、进度、弹窗等设计系统
│  ├─ pages/                                 # 11 个页面，按现有页面一一对应
│  ├─ features/                              # 页面状态与 invoke 封装
│  ├─ styles/                                # 设计令牌和页面样式
│  └─ types/                                 # 与 Rust DTO 对应的 TypeScript 类型
└─ src-tauri/
   ├─ Cargo.toml                             # Tauri 二进制与 workspace 根
   ├─ tauri.conf.json                        # 窗口、资源、NSIS、CSP 配置
   ├─ capabilities/                          # 最小 IPC 权限声明
   ├─ resources/                             # 打包资源映射，不保存机密
   ├─ crates/
   │  ├─ nwflash-domain/                     # 领域模型、纯策略、错误分类
   │  ├─ nwflash-application/                # 用例、操作闸门、生命周期编排
   │  ├─ nwflash-infrastructure/             # HTTP、下载、解压、校验、日志、偏好
   │  ├─ nwflash-windows/                    # adb/fastboot、Windows API、进程 IO
   │  └─ nwflash-tauri/                      # command、事件桥、应用状态、窗口控制
   └─ tests/                                 # Rust 集成测试与端到端契约测试
```

`Cargo.lock` 与 JavaScript lockfile 必须提交。依赖版本在首次脚手架时统一锁定；迁移期间不做无关升级。

### 4.2 前端和 Rust 的责任边界

React 是当前 WPF View 与视觉状态的替代，不是设备逻辑的第二份实现。

| 层 | 可以做 | 不可以做 |
| --- | --- | --- |
| React/TypeScript | 复刻布局、管理表单临时值、显示 DTO、发起用户意图、订阅后端事件、显示本地文件选择器结果。 | 保存 API token、直连 Cloudflare、直接调用 shell/文件系统、决定设备是否安全、拼接 adb/fastboot 命令、绕开确认或服务端门禁。 |
| Rust application/domain | 校验意图、构建执行计划、串行化和取消、设备/会话状态机、风险策略、认证状态与错误分类。 | 依赖 React 的具体组件或 CSS。 |
| Rust infrastructure/windows | HTTP、压缩格式、Range 下载、哈希、偏好、日志、子进程、Windows IO/注册表/路径。 | 持有 UI 生命周期或将原始系统错误直接拼入 UI。 |
| Tauri bridge | 将受控 DTO 进出 IPC，发布节流后的快照/事件，协调登录窗与主窗。 | 暴露通用 shell、任意文件读写或无权限的通用网络能力。 |

所有敏感网络请求由 Rust 使用 HTTPS 发出；前端不得使用 `fetch` 直接调用 `api.nwflash.cc.cd`。所有 Tauri command 参数均按不可信输入验证，即便该 command 只由本地前端调用。

### 4.3 页面与窗口映射

| WPF 对象 | Tauri 目标 |
| --- | --- |
| `LoginWindow.xaml` | 独立、固定尺寸的登录 Tauri 窗口；主窗在成功登录前不显示。 |
| `MainWindow.xaml` | 主 Tauri 窗口，保留自定义顶栏、左栏、右上状态/进度区与内容页。 |
| 11 个 `AppPage` 页面 | 11 个 React 页面组件；页面名称、排序、导航分组和默认页保持相同。 |
| `DriverReminderWindow`、`ResourceDownloadWindow`、`UpdateRequiredWindow` | 独立模态/子窗口或受控模态层；交互语义、按钮文案和取消规则保持一致。 |
| MVVM ObservableProperty/RelayCommand | 前端仅维护渲染状态；Rust command + 后端事件替代业务命令和可观察状态。 |

### 4.4 UI 保真规则

迁移不是重新设计。先从 WPF 提取颜色、字体、字重、间距、圆角、边框、阴影、按钮高度、页面宽度和响应式断点，形成 CSS 设计令牌；然后建立 `AppShell`、导航按钮、状态卡、进度块、表单字段、风险按钮、日志行、确认弹窗等可复用组件。

禁止先套用通用 UI 库主题再调整。截图对比必须覆盖登录窗、主窗各页面、空/加载/失败/运行中状态和四个关键模态窗；任一页面只能在保留现有文案和交互的基础上做修复性调整。

## 5. Rust 组件设计

### 5.1 领域和应用层

`nwflash-domain` 先迁移所有无 IO 模型、枚举和纯函数，包括分区风险、槽位计划、快速刷写计划、固件格式判定、路径/大小格式、设备状态、操作日志条目与 API DTO。

`nwflash-application` 实现以下长期状态机：

- `OperationCoordinator`：单飞操作、可取消 token、状态快照、进度节流、服务端许可、使用日志记录。
- `DeviceMonitor`：3 秒轮询、身份防抖、连续失败降级、操作完成补偿刷新。
- `SessionLifecycle`：登录、会话 ID、心跳、在线列表、强退、强制更新、登出和退出收尾。
- 各业务 use case：快速刷写、分区工作区、文件管理、投屏、固件提取、线刷、Root、资源安装和软件检测。

任何可改变设备状态的 use case 都只能经 `OperationCoordinator` 运行；UI 不能拥有另一条绕过它的执行路径。

### 5.2 平台和基础设施层

`nwflash-windows` 使用直接参数数组启动内置 `adb.exe`、`fastboot.exe`、scrcpy、payload_dumper 和驱动安装器，禁止经 `cmd.exe`/PowerShell 拼接不受控字符串。进程取消必须终止进程树；刷写与 payload 提取继续从 Windows 进程 IO 计数器取得进度，并保留“有 IO 即续期”的无进展超时逻辑。

`nwflash-infrastructure` 负责：

- 现有 Cloudflare API client 与错误映射；
- HTTP Range、多段下载、磁盘余量、staging、原子落盘和 100ms 进度节流；
- ZIP、tar、gzip、zstd、Vivo 特殊固件格式、payload_dumper 编排；
- SHA-256、APK/二进制身份校验、GitHub 镜像 failover；
- `%LOCALAPPDATA%`、`C:\\nwflash` 回退策略、偏好文件、操作/崩溃日志；
- 根目录资源的随包、外置和按需下载策略。

## 6. Cloudflare API 兼容性

服务端不修改，Rust client 必须按已有 `cloudflare/API.md` 与 C# 测试兼容以下端点：

- 登录和当前用户检查；
- `/api/rom`；
- `/api/heartbeat`、`/api/online`；
- `/api/operation/authorize`；
- `/api/usage/logs`；
- 版本检查及 401/403/404/402/426/429 等错误语义。

token 只保存在 Rust 进程内存，退出、登出、强制更新和强制退出时清除。前端只接收无凭据的视图 DTO。上游 VOTA token 继续只存在 Cloudflare Worker secret，永不进入新桌面端。

## 7. 安全与 VMP 发布设计

### 7.1 威胁模型和原则

假设攻击者可以阅读或修改 React 打包资源、调试/补丁本地 EXE、重复调用 IPC command，或截获其自己拥有的会话 token。不能假设客户端本地检查、隐藏 UI 或代码混淆能够独立授权用户。

因此安全按下列优先级实现：

1. **服务端授权优先**：保留登录、版本门禁、心跳、强退和每次操作 `authorize`；Rust 在执行设备副作用前强制检查许可。
2. **最小 IPC 权限**：不安装或暴露通用 shell、任意文件系统、任意 HTTP 等高权限前端插件；每个 command 有单一业务意图和显式 DTO 校验。
3. **浏览器面收缩**：严格 CSP；不加载远程脚本；禁用生产环境开发工具；前端不保存凭据/密钥。
4. **可执行文件防篡改**：VMP 保护少量经过专门烟测的 Rust 原生关键函数，并给产物和安装包签名。
5. **可追溯发布**：生成哈希清单、保存签名验证结果、保护前/后的自动烟测结果和版本映射。

### 7.2 VMP 边界

VMP 只针对最终 `nwflash.exe` 的原生代码。优先保护小而稳定、无跨线程回调/GUI/FFI 边界的函数：发行构建身份/完整性检查、服务端授权响应的解析与当前操作绑定、以及少量本地安全策略分派。它们用于提高逆向与补丁成本，不替代 Cloudflare 的最终授权。

不得把以下对象作为 VMP 虚拟化的首批目标：Tauri/Windows 入口、WebView 生命周期、Tokio runtime、React IPC 事件桥、长时间下载/解包循环、`adb`/`fastboot` 子进程控制、Windows 回调和第三方库。它们应保持可诊断，先用 mutation/常规保护或完全不保护；只有通过保护后烟测才逐步扩大范围。

不启用“检测到虚拟机就拒绝运行”一类策略，避免影响开发、测试、云桌面和普通用户；反调试、反篡改与保护失败只记录安全事件并走可理解的故障提示。

### 7.3 发布顺序

1. 锁定依赖，在干净 Windows x64 环境生成 release EXE 与前端静态资源。
2. 运行未保护版本的单元、集成、关键 UI 和安全回归测试。
3. 使用受版本控制的 `.vmp` 配置对最终 Rust EXE 进行选择性 VMP 处理；配置不保存 VMP 许可证、私钥或证书密码。
4. 对保护后的 EXE 再次进行完整烟测；VMProtect 修改二进制后，必须在此步骤之后做 EXE 的 Authenticode 签名和验签。
5. 以已签名 EXE 生成 NSIS 安装包；对最终安装包也签名和验签，生成 SHA-256 清单。
6. 在干净 Windows 10/11 机器安装、登录、检查资源、进行只读设备检测和 API 契约烟测；真实刷写测试使用专用测试设备。

Tauri Windows 安装包采用 NSIS；优先使用 WebView2 `embedBootstrapper`，以小幅体积换取缺少运行时环境的安装可靠性。应用使用每用户安装；仅 USB 驱动安装维持按需 UAC 提权。

## 8. 验证策略

### 8.1 Rust 测试迁移

将现有测试按风险迁移，而不是按文件名机械转换：

| 优先级 | 目标行为 |
| --- | --- |
| P0 | 操作协调器、取消/并发、服务端许可、设备状态、API 序列化与错误映射、分区风险/槽位/刷写计划。 |
| P1 | adb/fastboot 命令参数、IO 进度、进程树取消、Range 下载、磁盘检查、固件格式与提取、资源 SHA-256。 |
| P2 | 文件管理、投屏、Root、资源下载窗、在线列表、软件页与跨页面跳转。 |

所有网络测试使用本地 mock server；所有设备/进程测试使用可注入 runner 或录制夹具；不让单元测试连接真实设备、Cloudflare 或 GitHub。

### 8.2 UI 与发布验收

- 建立 WPF 基线截图和 Tauri 对应截图，以页面和状态矩阵进行人工审查；
- 自动验证导航、登录成功/失败、模态确认、禁用状态、进度事件节流、取消、登出和更新退出；
- 对 Rust command 进行参数越界、路径穿越、未登录、过期 token、被拒操作和重复 invoke 测试；
- 每个候选发布版本同时验证未保护、VMP 保护后、签名后、安装后四种产物状态。

## 9. 迁移次序

实施计划必须按以下阶段拆分，且每阶段可构建、可测试、可回退：

1. 建立 Tauri/React/Rust workspace、锁定构建工具、创建窗口和 UI 设计令牌；
2. 迁移领域模型、纯策略和 P0 Rust 测试；
3. 实现 Cloudflare client、登录/会话/操作协调/日志；
4. 先复刻 App Shell、登录、概览、日志、在线、软件等低设备风险 UI；
5. 迁移 adb/fastboot、设备监视、快速刷写、分区工作区和文件管理；
6. 迁移资源下载、固件提取、线刷、投屏和 Root；
7. 完成 UI 保真、端到端验收、发布脚本、VMP 试点与签名链；
8. 用灰度真机验证替换 WPF 发布物，之后才考虑删除旧工程。

每一阶段均要求行为测试通过、与旧客户端接口兼容、无未解决的高风险设备安全回归。任何真实刷写或 Root 验证必须在专用可恢复测试设备上执行。

## 10. 成功标准

- `cloudflare/**` 未被迁移或修改，且新桌面端对既有 API 契约兼容；
- 新客户端的可见页面、中文文案、信息架构和关键交互与现有 WPF 基本一致；
- 现有高风险业务规则有对应 Rust 自动化测试，关键命令不依赖前端作安全裁决；
- 最终 Windows 产物无需 .NET Runtime，可在目标 Windows 环境安装运行；
- VMP、EXE/安装包签名和保护后烟测形成可重复发布链；
- 真机验收覆盖设备检测、只读查询、刷写取消/失败收尾和至少一个经批准的完整刷写回归流程。
