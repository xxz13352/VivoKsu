# 奶蛙Flash(NWflash)项目架构文档

> Vivo 手机刷机 / Root 工具箱。Windows WPF 桌面应用(.NET 8) + Cloudflare Worker API + Web 后台。
> 本文描述系统整体架构、模块职责、关键数据流与设计决策。业务细节见 [safeflash-ota.md](safeflash-ota.md)、[../cloudflare/API.md](../cloudflare/API.md)。

## 命名约定(2026-08-14 定稿)

| 场景 | 名称 |
| --- | --- |
| **客户端显示名**(用户可见) | **奶蛙Flash**(所有窗口标题 / 品牌文案 / MessageBox 统一显示这个) |
| **代码 / 工程 / 服务名** | **NWflash**(程序集、命名空间、文档、目录路径) |
| **缩写 / 品牌标记** | **NWF**(窗口角落方块标记) |
| **域名 / Worker** | `nwflash.cc.cd`、`api.nwflash.cc.cd`、`web.nwflash.cc.cd`、`nwflash-rom`、`nwflash-web` |
| **API 版本头** | `X-Nwflash-Version` |
| **桌面端工程路径** | `src/VivoKsu.App`(历史遗留,暂不改名) |

> ⚠️ **不要**把客户端 UI 文案写成 Nwflash——用户看到的应是「奶蛙Flash」。技术标识(NWflash / X-Nwflash-Version / nwflash.cc.cd)与显示名是两套,各归其位。

## 目录

1. [系统总览](#1-系统总览)
2. [仓库结构](#2-仓库结构)
3. [桌面端架构](#3-桌面端架构)
4. [API 服务(Cloudflare Worker)](#4-api-服务cloudflare-worker)
5. [Web 后台](#5-web-后台)
6. [数据流与关键时序](#6-数据流与关键时序)
7. [关键设计决策与踩坑](#7-关键设计决策与踩坑)
8. [测试](#8-测试)
9. [发布与内置组件](#9-发布与内置组件)
10. [已知限制](#10-已知限制)

---

## 1. 系统总览

**商业定位**:Nwflash 是**商业付费工具** —— 桌面端启动必须用后台创建的账号登录(付费授权),未登录不可进入主界面;**登录后即可使用,不做按次扣点 / 配额计费**(上游 VOTA 的信用点由运营方承担,见 §4.5);每次 ROM 查询按用户审计。**服务端 100% 托管在 Cloudflare**(Workers + D1),**零自有服务器**:认证、版本授权、计费、审计与后台管理全部在 Cloudflare Edge。

系统分三层,**上游 VOTA 凭据只在 Worker 上,从不进入桌面端**:

```mermaid
flowchart LR
    subgraph Desktop["桌面端 (VivoKsu.App · WPF .NET8)"]
        UI["MainWindow + 9 个页面\n(登录门禁后进入)"]
        CORE["AppComposition 组合根\n会话 / 监视 / 协调器 / 各页面 VM"]
        FB["FastbootCliRunner(唯一 fastboot.exe)\n+ FastbootRsBackend(adb)"]
    end

    subgraph Cloudflare["Cloudflare Edge"]
        API["api.nwflash.cc.cd\nWorker nwflash-rom"]
        WEB["web.nwflash.cc.cd\n后台管理 Worker"]
        USER["user.nwflash.cc.cd\n用户门户 Worker"]
        DB[("D1: nwflash-db\napi_users / app_versions\naccess_logs / admins")]
    end

    subgraph Upstream["上游"]
        VOTA[("VOTA API\napi.otau.cc.cd\n持有 OTA 链接 + 信用点")]
    end

    UI --> CORE
    CORE --> FB
    UI -->|"登录 /api/login · ROM 查询 /api/rom"| API
    API --> DB
    API -->|"Bearer VOTA_API_TOKEN\nresolve_url / resolve_flash_url"| VOTA
    WEB --> DB
    API -. 共用 .-> DB
```

- **桌面端**持有账号密码或本地 token,通过 `api.nwflash.cc.cd` 登录、查询 ROM 链接,直接下载并刷写。
- **Worker** 唯一持有 VOTA API Token(secret),校验登录、做 Nwflash 版本门禁、转发到 VOTA 并记访问日志。
- **Web 后台**管理用户 / 版本 / 日志,与 API 共用同一个 D1 数据库。

### 关键原则

| 原则 | 落地 |
| --- | --- |
| **凭据隔离** | VOTA Token 只存 Worker secret;桌面端代码无 `api.otau.cc.cd` 与 token |
| **商业门禁** | 桌面端启动必须登录(`/api/login`);`/api/rom` 强制带 token,封禁用户 403 |
| **版本控制** | 后台「版本号控制」登记 Nwflash 客户端版本;版本低于最低版本 → 服务端 **426 强制更新** |
| **按用户审计** | 每次 ROM 查询写 `access_logs`(用户 / PD / 版本 / URL / 状态) |
| **任务原子性** | 所有耗时操作经 `OperationCoordinator` 串行、可取消、失败即停、进度 100ms 节流 |
| **零自有服务器** | 全部后端跑在 Cloudflare Workers + D1;无任何自托管服务端代码 |

---

## 2. 仓库结构

```
VivoKsu 工具/
├─ VivoKsu.slnx                     # 解决方案(src 1 项目 + tests 1 项目)
├─ src/
│  ├─ VivoKsu.App/                  # 桌面应用(net8.0-windows, WPF)
│  │  ├─ App.xaml(.cs)              # 启动门禁 / 崩溃日志 / 退出清理
│  │  ├─ MainWindow.xaml(.cs)       # 单窗口多页面导航 + 全部 XAML
│  │  ├─ LoginWindow.xaml(.cs)      # 登录窗口(taste 风格)
│  │  ├─ Models/                    # 领域模型(AppPage / 分区 / payload / 快照 / 日志…)
│  │  ├─ ViewModels/                # 各页面 MVVM 视图模型(15 个)
│  │  ├─ Services/                  # 业务服务与基础设施(40+ 个)
│  │  ├─ apk/                       # KernelSU 安装包(KSU.APK / KernelSU.apk)
│  │  ├─ payload-tools/             # payload_dumper.exe(payload-dumper-rust)
│  │  ├─ platform-tools/            # adb.exe + 唯一 fastboot.exe(带进度)
│  │  ├─ root-tools/                # magiskboot.so
│  │  └─ scrcpy/                    # scrcpy(发布脚本自动补齐)
├─ cloudflare/                      # Worker + 后台 + 用户门户 + 官网(见 §4 §5)
├─ tests/
│  └─ VivoKsu.App.Tests/            # 桌面应用单元测试(约 50 个文件)
├─ scripts/
│  ├─ Publish-Release.ps1           # 一键发布 self-contained win-x64
│  ├─ Ensure-Scrcpy.ps1             # 发布前自动获取 scrcpy
│  └─ verify-*.ps1                  # UI 自动化验证脚本(UIA 导航 + 截图像素采样)
├─ docs/                            # architecture.md(本文) / safeflash-ota.md
```

---

## 3. 桌面端架构

### 3.1 启动流程与登录门禁

入口在 [App.xaml.cs](src/VivoKsu.App/App.xaml.cs):

```mermaid
flowchart TD
    A[App.OnStartup] --> B[注册崩溃日志<br/>DispatcherUnhandledException → crash.log]
    B --> V["BlockForForcedUpdate<br/>版本门禁:过低 → 更新窗 + 退出"]
    V --> S["ShutdownMode = OnExplicitShutdown<br/>关窗不自动退出,由代码显式 Shutdown"]
    S --> L["RunApplicationLoop()<br/>登录循环(登出后可重入)"]
    L --> C["LoginWindow.ShowDialog<br/>每次启动强制登录"]
    C -->|取消/关闭| G["Shutdown()<br/>不进主界面"]
    C -->|登录成功| F["token = login.Token"]
    F --> H["AppComposition.CreateDefault()<br/>LogoutRequested += 事件<br/>StartSessionAsync(token, username)"]
    H --> I["MainWindow.Show()<br/>Closed → OnMainWindowClosed"]
    I -->|X 关闭| X["Shutdown()"]
    I -->|点击登出| O["MainViewModel.LogoutCommand<br/>(忙时禁用)"]
    O --> E["AppComposition.OnLogoutAsync<br/>StopAsync:心跳 goodbye + 上传使用日志 + 停监视/协调器"]
    E --> R["LogoutRequested 事件"]
    R --> L
```

要点:

- **每次启动强制登录**:无本地免登录 —— 每次启动必弹登录窗,`/api/login` 通过才进主界面;token 不持久化,仅本次会话注入 `OtaApiClient`([LoginService.cs](src/VivoKsu.App/Services/LoginService.cs) 用 `ConfigureAwait(false)`,不依赖 UI 上下文)。
- **同进程登出(2026-08-15 新增)**:点「登出」→ `MainViewModel.LogoutCommand`(`CanExecute = !Coordinator.IsBusy`,刷写/传输运行中禁用,防打断设备操作,与 force-exit「刷写中先取消等 Idle 再退」一致)→ `AppComposition.OnLogoutAsync`(`StopAsync` 优雅下线后抛 `LogoutRequested` 事件)→ App 关主窗 → `OnMainWindowClosed` 识别登出态(`isLogout`)重入 `RunApplicationLoop` 弹登录窗。旧 composition 已 `StopAsync`(`stopped` 幂等守卫,登出后 OnExit 直接返回),新登录全新构造,无状态残留。
- **退出清理**:`OnExit` 用 `DispatcherFrame` 泵消息最多 5s 等 `composition.StopAsync()` 完成 —— 清理下载的 Vivo 临时 gzip 与各盘 `Nwflash\safe-flash` staging,并停监视/镜像进程。
- **崩溃日志**:未捕获异常写 `%LOCALAPPDATA%\Nwflash\crash.log`(商业工具排查用)。
- **登录后程序消失修复**:`ShowDialog` 关闭触发 `OnLastWindowClose` 退出 —— 改为主窗口显式 `Closed` 处理器(登出重入循环 / 否则 `Shutdown()`),并设 `ShutdownMode = OnExplicitShutdown`。

### 3.2 组合根与依赖组装(无第三方 DI)

[AppComposition.cs](src/VivoKsu.App/Services/AppComposition.cs) 手写组合根,一次构建整个对象图:

```mermaid
flowchart LR
    subgraph infra["基础设施"]
        NATIVE["FastbootRsApiFactory\n(adb: PlatformToolsNativeApi / adb.exe)"]
        PROC["SystemProcessRunner"]
        PREFS["ToolPathPreferences"]
    end
    subgraph core["核心会话"]
        BACKEND["FastbootRsBackend"]
        SESSION["DeviceSessionViewModel"]
        LOGS["OperationLogService"]
        COORD["OperationCoordinator"]
        MONITOR["DeviceMonitorService"]
    end
    subgraph pages["页面 VM"]
        OVER["Overview"]
        QF["QuickFlash"]
        MIR["Mirror"]
        FM["FileManager"]
        LF["LineFlash"]
        ROOT["Root"]
        FE["FirmwareExtract"]
        SF["SafeFlash"]
    end
    MAIN["MainViewModel"]

    NATIVE --> BACKEND
    BACKEND --> SESSION
    SESSION --> COORD
    LOGS --> COORD
    MONITOR --> SESSION
    COORD -->|StateChanged → 补偿刷新| MONITOR

    OVER & QF & MIR & FM & LF & ROOT & FE & SF --> MAIN
```

关键接线:

- `Monitor.DeviceRefreshed += MainViewModel.OnDeviceRefreshedAsync` —— 设备身份变化时触发(页面事件)。
- **跨页面联动**:固件提取 / Root 产物的「刷入此镜像」回调把镜像交 `QuickFlash.PreparePatchedImage` 并切到快速刷写页。
- `SetAuthToken(token)` 把登录 token 注入 `OtaApiClient`(后续 `/api/rom` 带 `Authorization: Bearer`)。

### 3.3 MVVM 与页面导航

单窗口 + 左侧导航,`MainWindow.xaml` 用一个 `Grid` 按 `SelectedPage` 做 `DataTrigger` 显隐切换页面。左侧导航**按刷机链路分组**(设备概览 / 文件管理 / ADB 投屏 ‖ 快速刷写 / 可视刷写 / VIVO 线刷 / 固件提取 / Vivo ROOT ‖ 在线状态 / 软件),左下角固定**账号栏**(登录账号 `AccountName` + 每秒走动的时钟 `CurrentTimeText` + 登出按钮 `LogoutCommand`)。右侧固定 **DEVICE STATUS 卡片**(设备信息 + **右上角统一「操作进度」区**)。

| AppPage 枚举 | 页面 | 核心 ViewModel |
| --- | --- | --- |
| `Overview` | 设备概览 | `OverviewViewModel` |
| `FastbootFlash` | 快速刷写 | `QuickFlashViewModel` |
| `AdbActions` | ADB 投屏 | `MirrorViewModel` |
| `RootTools` | Vivo ROOT | `RootViewModel` |
| `FileTransfer` | 文件管理 | `FileManagerViewModel` |
| `LineFlash` | 可视刷写 | `LineFlashViewModel` + `PartitionWorkspaceViewModel` |
| `FirmwareExtract` | 固件提取 | `FirmwareExtractViewModel` |
| `SafeFlash` | VIVO 线刷 | `SafeFlashViewModel` |
| `OperationLog` | 操作日志 | `OperationLogViewModel` |
| `OnlineStatus` | 在线状态 | `OnlineViewModel` |
| `Software` | 软件 | `SoftwareViewModel` |

技术底座:**CommunityToolkit.Mvvm 8.4**(`[ObservableProperty]` / `[RelayCommand]`)+ HandyControl 3.5.1 + teal 主题(参考 taste-skill 审美迭代)。

> **右上角统一「操作进度」区(2026-08-15)**:右侧 DEVICE STATUS 卡片内的进度区是唯一主进度显示,五块按「操作是否在运行」显示(切换页面进度不消失):
>
> | 块 | 显示条件 | 绑定 |
> | --- | --- | --- |
> | 快速刷写 | `QuickFlash.IsFlashOperationActive` | 当前分区 + 当前条 + 总进度 + 速度 |
> | 可视刷写 | `PartitionWorkspace.IsExecuting` | 当前分区 + 当前条 + 总进度 + 速度 |
> | VIVO 线刷 | `SafeFlash.IsBusy` | 当前分区 + 当前条 + 总进度 + 速度 |
> | 固件提取 | `FirmwareExtract.IsPayloadBusy` | 当前分区 + 总进度 + 速度·耗时(独立于协调器) |
> | 设备操作(通用) | `DeviceSession.IsBusy` 且上四块均空闲 | 阶段文案 + 不定进度(文件传输 / ROOT / 安装等只报阶段) |
>
> 全部空闲时显示「无进行中的操作」占位(读取分区表期间不显示)。固件提取页与文件管理页原有的重复进度条已移除;可视刷写分区行级小进度条与「读取分区表」加载条保留原位。

### 3.4 设备监视与会话(三通道 + 防抖)

[DeviceMonitorService.cs](src/VivoKsu.App/Services/DeviceMonitorService.cs) 用 `PeriodicTimer`(默认 3s)轮询,三种入口:

1. **心跳轮询** `RefreshHeartbeatAsync` —— 每 tick 刷新会话(电量/型号等),**仅在设备身份(连接状态/串号)变化时才触发下游 `DeviceRefreshed`**。
2. **手动刷新** `RefreshManualAsync` —— 用户点「刷新设备」,`forceFire:true` 必触发下游。
3. **补偿刷新** —— 监听 `coordinator.StateChanged`,操作结束(从 busy→idle)后自动补一次刷新。

```mermaid
sequenceDiagram
    participant T as PeriodicTimer(3s)
    participant M as DeviceMonitorService
    participant S as DeviceSessionService
    participant V as DeviceSessionViewModel
    participant H as DeviceRefreshed handlers

    T->>M: RefreshHeartbeatAsync
    M->>M: refreshGate 单飞 + coordinator.IsBusy 跳过
    M->>S: RefreshAsync(Automatic)
    S->>V: Discover(adb/fastboot) + 读详情/电量
    Note over M: identityBefore 比对
    alt 身份变化 或 forceFire
        M->>H: DeviceRefreshed(只做镜像协调等非分区表更新)
    else 未变化
        Note over M: 不触发下游(避免分区表反复重读)
    end
```

设计要点:

- **分区表只在用户点击「读取分区表」时读** —— 心跳 / 补偿刷新都不会重读,防止刷写中途被反复读取打断。
- **自动断开防抖**:连续 2 次心跳测到 disconnected 才真正断(瞬时抖动不误拉)。
- **连续失败降级**:自动刷新连续失败 3 次 → 记错误并标记未连接,避免「已连接」幻影设备让用户操作全静默失败。
- 刷新经 `SynchronizationContext` 回 UI 线程;`refreshGate` 保证不并发。

### 3.5 操作协调器与日志

[OperationCoordinator.cs](src/VivoKsu.App/Services/OperationCoordinator.cs) 是所有耗时操作的门:

- `RunAsync(kind, title, operation)` —— `SemaphoreSlim` 串行化;`CancellationTokenSource` 可取消;`StateChanged` 事件广播。
- 进度 `Report(stage, progress)` 分两种:**阶段变化**立即广播 + 写日志;**纯进度变化** 100ms 节流。
- 结束状态自动写日志(完成 ✅ / 取消 ⚠ / 失败 ❌)。
- `DeviceSessionViewModel` 的 `IsBusy` / `StatusText` / `ConnectionAccentBrush` 由协调器驱动,UI 据此禁用按钮。

[OperationLogService.cs](src/VivoKsu.App/Services/OperationLogService.cs) 按级别(Info/Success/Warning/Error)记录所有操作;操作日志页用 `[HH:mm:ss] 消息` 单行等宽显示并自动滚底。

### 3.6 后端抽象(adb + 唯一 fastboot.exe)

fastboot-rs 原生 DLL 已整体移除(错误码不可读、无刷写进度)。刷写后端统一为一个 **`FastbootCliRunner`**,指向唯一 `platform-tools/fastboot.exe`(35.0.2-eng,带进度),承担**全部** fastboot 操作并带**连续传输进度**:

```
FastbootCliRunner (唯一 fastboot.exe)
├── FlashAsync(serial, partition, image, progress)  连续进度:GetProcessIoCounters 采样 写字节/镜像大小
├── GetVarAsync / PartitionExistsAsync              剥离 (bootloader) 前缀;区分「无分区」vs「传输失败」
├── EraseAsync / RebootAsync / SetActiveAsync
└── 错误 = exit code + 输出文本(可读,不再有 C ABI 错误码)
```

- **`FastbootRsBackend`**(经 `IFastbootRsNativeApi` / `PlatformToolsNativeApi`)仅保留 **ADB 能力**:设备发现 / shell / 文件传输 / 安装 / adb reboot。
- **进度**:flash 用「无进展超时」+ 每 250ms 采样 `GetProcessIoCounters`,`进度 = 进程写字节 / 镜像大小`(慢速 USB 刷 4-6GB 大分区不被强杀,且有真实百分比)。
- **超时策略**:flash 无进展超时 600s;getvar 探测 20s 墙钟(非零退出区分「无分区」vs「传输失败」);erase/reboot/set_active 60s 墙钟。

> 为什么统一 CLI?fastboot DLL 的 `fastboot_flash` C ABI 只回粗错误码(多数失败归 `-8`),getvar/reboot 把所有失败压成 `-4`,拿不到原因;CLI 打印可读错误(无设备 + 检查清单 / 镜像未找到:<路径> / 设备 FAIL 消息),且能采样出真实进度。

### 3.7 分区传输抽象

`IPartitionTransport` 封装两种通道,可视刷写按连接状态自动选择:

```mermaid
flowchart LR
    PART["PartitionExecutionService<br/>(依序执行选中分区)"]
    COORD["OperationCoordinator(进度)"]
    PART --> COORD
    PART -->|fastboot 在线| FB["FastbootPartitionTransport<br/>getvar / flash / erase"]
    PART -->|adb root 在线| AR["AdbRootPartitionTransport<br/>adb root + dd 读写"]
    AR --> RUNNER["AdbRootTransferRunner<br/>(逐参数 + 2>/dev/null;取消杀进程树)"]
```

要点:

- **`AdbRootPartitionTransport`** 用 `dd` 走 adb root 通道读/写分区;`EnsurePartitionUnchangedAsync` 按 `ResolveByNameTemplate` 重解析 by-name 多布局设备。
- **`PartitionExecutionService`** 依序执行,进度经 `OperationCoordinator` 100ms 节流上报;首个失败分区即停。
- 备份有**完整性校验**:`task.SizeBytes` 必须等于备份文件长度,否则抛错(防备份不完整误判成功)。

### 3.8 固件提取流水线

[FirmwarePartitionExtractor.cs](src/VivoKsu.App/Services/FirmwarePartitionExtractor.cs) 按 zip 内是否含 `payload.bin` 分流;`FirmwareFormatDetector` 按魔数(由提取器内联判断)分流:

```mermaid
flowchart TD
    SRC[本地 zip / payload.bin / 云端直链] --> FMT{格式?}
    FMT -->|"PK zip 含 payload.bin<br/>(AOSP payload OTA)"| PD["payload_dumper.exe<br/>HTTP Range 按需读 blob"]
    FMT -->|"PK zip 直接镜像"| ZIP["ZipFile 解 *.img / *.bin"]
    FMT -->|"gzip 1f8b(Vivo 专用)"| GZ["VivoFirmwareExtractor<br/>流式 gzip→tar"]
    PD --> OUT[分区镜像列表]
    ZIP --> OUT
    GZ --> OUT
```

**实时进度(重点)**:payload_dumper 不输出流式进度,且其网络读取(Rust reqwest 走 IOCP/AFD)不计入进程 `ReadTransferCount`。可靠信号是**进程写入字节数 `WriteTransferCount`** —— 后台每 200ms 采样 `GetProcessIoCounters`,按分区 `size_in_bytes` 作分母,得到真实连续进度条与速度。Vivo gzip 路径以已解压字节 / gzip 总量直接报进度。

- **块式内容检测**:`HasBlockBasedContent` 识别 `.new.dat` / `.patch.dat` / `.transfer.list`(块式分区暂不支持刷),确认弹窗给警告。
- **`PayloadDumperRunner`**:同样是「无进展超时」(监控进程 I/O),不再 120s 硬超时杀解包。

### 3.9 安全刷写(VIVO 线刷)全链路

[SafeFlashViewModel.cs](src/VivoKsu.App/ViewModels/SafeFlashViewModel.cs) 的核心流程(详见 [safeflash-ota.md](safeflash-ota.md)):

```mermaid
flowchart TD
    A[读设备<br/>adb PD / 版本号] --> B{选源}
    B -->|云端| C["OtaApiClient /api/rom<br/>(带 token)"]
    C --> D["OtaDownloadService<br/>bezzad 多分片下载<br/>staging 优先系统 SSD"]
    B -->|本地| E[选 .zip / .payload]
    D --> F["FirmwarePartitionExtractor 解包<br/>过滤 preloader*/lk"]
    E --> F
    F --> G[内联确认<br/>含块式内容警告]
    G --> H["adb reboot fastboot → fastbootd<br/>(1.5s 稳定等待)"]
    H --> I["逐个 fastboot flash<br/>getvar partition-type 预检跳过缺失分区"]
    I --> J["fastboot reboot"]
    J --> K["日志面板 [HH:mm:ss] 逐行<br/>Google-fastboot 风格"]
```

要点:

- **三种 Vivo OTA 结构**:payload OTA(PD2417)/ 直接镜像 zip(PD2057)/ firmware-update 镜像(PD2196),块式 OS 分区只警告不刷。
- **`WaitForFastbootAsync` 直接轮询 `backend.DiscoverAsync`**(不依赖冻结的会话快照),避免 ADB→fastbootd 过渡时检测不到设备。
- **分区存在性预检**:每个分区 flash 前 `getvar partition-type:<name>`,设备没有的分区跳过 + 日志,避免未知分区中止半刷。
- **staging 清理**:取消 / 失败 / 退出都清理 staging;盘选择优先系统 SSD(≥15GB),bezzad 多分片随机写 HDD 会停滞。
- **下载内存有界 + 进度节流**:bezzad 多分片缓冲设 **256MB 上限**(库对 ≤0 视为无上限,网速快于磁盘时会无界堆积到包大小量级 OOM);进度上报 **~100ms 节流**(下载完成事件必达,不丢 100%)。
- **进度分段**:解包 0–0.5、刷写 0.5–1,不重叠;右侧栏当前分区行显示 `百分比 · 速度 MB/s`。

### 3.10 其它关键服务

| 服务 | 职责 |
| --- | --- |
| `VivoRootResourceService` | Root 管理器 APK 校验(**SHA-256 白名单** + AndroidManifest.zip 检查,防被替换) |
| `VivoVendorBootProcessor` | vendor_boot 补丁处理(官方 / GKI 内核),GKI 缺失跳过;输出已存在先删再重试 |
| `NwflashDevicePatchService` | 设备 patch 应用(经 adb root) |
| `QuickFlashService` | 快速刷写:`is-userspace` getvar 失败降级不抛;`expectedSerial` 防串号错刷;**一律 fastbootd**(不再提供 Fastboot/bootloader 选择) |
| `MirrorService` + `ScrcpyProvisioningService` | scrcpy 投屏;启动用 **`ADB` 环境变量** 指向内置 adb(scrcpy v4.0 移除了 `--adb-path`);自动投屏开关关闭时取消在途协调;`.staging` 目录清理 |
| `AdbFileService` | ADB root 通道文件浏览 / 上传 / 下载 / 删除 |
| `DeviceInfoService` | 读设备详情(版本 / 电量 / 型号) |
| `ToolPathPreferences` | 本地设置(settings.json),含登录 token 持久化 |

---

## 4. API 服务(Cloudflare Worker)

`api.nwflash.cc.cd` = Worker `nwflash-rom`(`cloudflare/src/index.ts`)。完整接口契约见 [cloudflare/API.md](../cloudflare/API.md)。

### 4.1 端点

| 端点 | 方法 | 说明 |
| --- | --- | --- |
| `/health` | GET | 健康检查 |
| `/api/app/version?current=` | GET | Nwflash 版本策略(免登录,启动强制更新拦截) |
| `/api/login` | POST | 账号密码 → API token(桌面端登录) |
| `/api/me` | GET | 校验 token 有效性(桌面端每次强制登录,不再用于免登录) |
| `/api/rom?pd=&version=` | GET | 解析 OTA 直链(强制登录 + 版本门禁 + 记日志) |

### 4.2 认证与授权模型

```mermaid
flowchart TD
    R["/api/rom 请求"] --> A{Authorization Bearer?}
    A -->|无| 401["401 请先登录"]
    A -->|有| B{api_users 查询<br/>token 匹配}
    B -->|无/停用| 401B["401 token 无效或已停用"]
    B -->|banned=1| 403["403 账号已被封禁"]
    B -->|有效| V{"X-Nwflash-Version<br/>低于最低版本?"}
    V -->|是| 426["426 强制更新"]
    V -->|否| D["代理 VOTA resolve_url"]
    D --> E["200 {url,...} + 写 access_logs"]
```

- **桌面端登录**(`/api/login`):`api_users.username` + PBKDF2-SHA256(100k 迭代)校验密码,成功返回该用户 token;封禁 / 停用 / 未设密码分别报错。
- **`/api/me`**:token → `{loggedIn, name}`(校验 token;桌面端已改为每次强制登录,不再调用它免登录)。
- **版本门禁**:所有请求带 `X-Nwflash-Version`;低于后台「版本号控制」最低版本 → **426 强制更新**;启动时走免登录的 `/api/app/version`。
- **`/api/rom`**:强制 token;**封禁用户 403**,版本门禁 426,成功 200 并记日志。

### 4.3 D1 数据模型(`nwflash-db`)

| 表 | 用途 | 关键列 |
| --- | --- | --- |
| `admins` | 后台管理员 | username / salt / password_hash |
| `admin_sessions` | 后台会话(7 天 cookie) | admin_id / token / expires_at |
| `api_users` | 客户端账号 = 桌面登录账号 | username(唯一) / name / token / password / salt / enabled / **banned** |
| `app_versions` | Nwflash 版本控制(强制更新) | version / min_version / download_url / enabled,`UNIQUE(version)` |
| `access_logs` | 每次 ROM 查询审计 | api_user_id / api_user_name / pd / version / url / status |

### 4.4 错误映射

| 上游 `code` | HTTP | 语义 |
| --- | --- | --- |
| `NOT_FOUND` / `not found` 文本 | 404 | 平台无此版本记录 |
| `AUTH_FAIL` | 401 | VOTA 认证失败 |
| `INSUFFICIENT_CREDITS` | 402 | 信用点不足 |
| `FORBIDDEN` | 403 | VOTA 拒绝(VER 白名单等) |
| `RATE_LIMITED` | 429 | 请求过频 |
| 其它 / 连不上上游 | 502 | 上游异常 |

### 4.5 上游计费 / 信用点(运营方成本)

每次成功 `resolve_url` 扣 **1 信用点**;`resolve_flash_url`(线刷)扣 **3 信用点**。信用点归属 **Worker 所持 VOTA token 的账户(运营方)**。这是 Nwflash 运营方在上游 VOTA 的成本,**不对 Nwflash 用户做任何扣点 / 按次计费** —— 用户只要登录即可查询;`record not found` / 参数错误不扣点。

### 4.6 商业运营闭环

```mermaid
flowchart LR
    ADMIN["web.nwflash.cc.cd<br/>创建账号(用户名+密码+token)<br/>版本开关 / 封禁 / 停用"] --> API["api.nwflash.cc.cd"]
    API -->|登录门禁| DESK["桌面端登录"]
    DESK -->|"/api/rom 带 token"| API
    API -->|每次查询| AUDIT["access_logs 按用户审计"]
    API -->|扣上游信用点| BILL["VOTA 账户计费(运营方成本)"]
    BILL -->|额度不足 402| DESK
    API -->|版本过低 426| DESK
    API -->|封禁 403| DESK
```

- **授权载体**:`api_users` 账号 = 登录凭证 + API token + `enabled`/`banned`。桌面端登录拿 token,ROM 查询凭 token。
- **版本授权**:`app_versions` 表登记 Nwflash 客户端版本,启用的最高版本为当前策略;客户端低于 `min_version` → 服务端 426 强制更新,后台可随时开关。
- **审计闭环**:每次查询写 `access_logs`,后台可查谁在何时查了哪个版本、成功与否。
- **用户不按次计费**:Nwflash 用户登录即可查询、不限次数;上游扣的是运营方账户的信用点(§4.5),`402` = 运营方上游余额不足,客户端提示「服务端信用点不足」。
- **处罚通道**:后台封禁 / 停用 → 登录 `401`、查询 `403`,**即时生效**——token 无本地缓存,天然可吊销。

---

## 5. Web 后台

`web.nwflash.cc.cd`(`cloudflare/web/src/index.ts` + 单文件 SPA `admin.html`,详见 [cloudflare/web/README.md](../cloudflare/web/README.md)):

- **界面(2026-08 重写,「固件登记簿」)**:机加工纸面画布 + 发丝刻线 + 单一账簿蓝的系统控制台。**五个菜单** —— 版本号控制 / 用户管理 / 访问日志 / **在线状态(LIVE)** / **使用日志**;**改密降级为头部维护按钮,不是第六菜单**。
  - **服务健康带**:Nwflash 当前版本 / API 用户 / **在线人数** / 近 24h 查询 / 近 24h 失败(客户端 best-effort 统计,基于最近 500 条日志)。
  - **Nwflash 版本控制**:登记版本号(版本 / 最低版本 / 下载地址)→ № 页边码登记册 + 双墨状态(● 启用 / ○ 停用)+ 当前策略结算;客户端低于「最低版本」→ 强制更新。
  - **用户管理**:建号 → **撕口一次性 token 凭证**(可复制);重置密码 / 换 token / 封禁 / 停用 / 删除。
  - **在线状态**:实时会话登记册(显示名 + 登录账号 / 版本 / IP / 上线 / 最后心跳 / 在线时长),每 10s 刷新;**强制下线**给会话打 `force_exit`,客户端下一个心跳(≤5s)退出进程(刷写中先取消、等 Idle 再退,不打断分区写入)。kick 写 `admin_audit_log` 审计。
  - **使用日志**:客户端每次用户操作运行前经 `POST /api/operation/authorize` 许可(默认放行、封禁/停用拒绝),执行后批量上传 `POST /api/usage/logs`;后台按 `operation_kind` 分类查看(分类/状态筛选 + 分页)。
  - **访问日志**:带列标尺的查询读出口,OKAY / FAIL 双墨,URL 断行省略。
  - **操作反馈以 OKAY/FAIL/INFO 协议行回显** —— 登记版本 / 建用户 / 换 token 都写成协议行,操作历史即审计轨迹。
- **功能**:管理员登录、Nwflash 版本控制(强制更新)、API 用户管理(建号 / token 生成轮换 / 停用 / 封禁)、访问日志。
- **安全**:强制 HTTPS + HSTS + CSP + HttpOnly/Secure 会话 Cookie + PBKDF2-SHA256 密码哈希 + 随机 session token;首启用 `ADMIN_SEED_PASSWORD` 播种初始管理员。
- 与 `api.nwflash.cc.cd` **共用同一 D1 `nwflash-db`** —— API 侧执行版本校验 / 认证 / 记日志,后台负责管理。

### 5.1 用户门户 `user.nwflash.cc.cd`(客户自助后台)

`user.nwflash.cc.cd`(`cloudflare/user/src/index.ts` + 单文件 SPA `user.html`,高级白 + 毛玻璃设计):

- **面向授权客户**的自助表面,与桌面端同源 API token(`api_users.token`)鉴权;登录(账号+密码)在本 worker 内完成(PBKDF2,与 api/web 同算法)。
- **我的查询日志**:该用户的 `access_logs`(PD / 版本 / 状态 / URL),分页 + PD 过滤。
- **在线会话**:该用户的 `online_sessions`(版本 / IP / 上线 / 时长),**⟠ 强制下线**(仅限本人会话,设 `force_exit`)。
- **修改密码**:校验当前密码后更新(`api_users.salt + password`)。
- 写操作校验 `X-Requested-With`(CSRF 兜底);安全头与 admin 一致(HSTS / CSP / no-store)。

### 5.2 官网 `nwflash.cc.cd`(营销落地页)

`nwflash.cc.cd`(`cloudflare/website/src/index.html` + Worker `nwflash-site`,高级白 + 液态玻璃设计,详见 [cloudflare/website/README.md](../cloudflare/website/README.md)):

- **对外营销面**,不鉴权、无 D1;Worker 仅托管单页 `index.html`(任何路径统一返回),安全头与后台一致。
- **设计语言**:高级白 + 液态玻璃(磨砂半透明白玻璃面板 + `backdrop-filter` 高斯模糊 + 内高光 + 发丝描边,Apple Liquid Glass 网页近似);单一深青强调;数据等宽 mono。
- **区块**:HERO(动能排字 + 液态玻璃设备控制台迷你 UI)、功能 Bento、VIVO 线刷五步、技术栈条、更新日志、下载 CTA、页脚。
- **更新日志**:在官网单页内维护(发版时在 `index.html` 更新日志区块登记版本号与变更)。
- 部署:`cd cloudflare/website && npx wrangler deploy`(绑定根域 `nwflash.cc.cd`)。

---

## 6. 数据流与关键时序

### 6.1 登录 → 查询 ROM → 下载 → 刷写

```mermaid
sequenceDiagram
    participant U as 用户
    participant A as App(登录门禁)
    participant API as api.nwflash.cc.cd
    participant D as D1
    participant V as VOTA
    participant SF as SafeFlash

    U->>A: 输入账号密码
    A->>API: POST /api/login {username,password}
    API->>D: 查 api_users + PBKDF2 校验
    API-->>A: {ok, token}
    A->>SF: 存 token(OtaApiClient.Token)
    U->>SF: 下载+刷入
    SF->>API: GET /api/rom?pd=PD2417&version=16.2.12.0.W10.V000L1
    API->>D: 校验 token + X-Nwflash-Version(版本门禁)
    API->>V: POST resolve_url(Bearer VOTA token)
    V-->>API: {url, ...}
    API->>D: INSERT access_logs
    API-->>SF: {url}
    SF->>SF: 下载 → 解包 → fastbootd 刷写 → reboot
```

### 6.2 设备心跳与操作互斥

```mermaid
sequenceDiagram
    participant T as PeriodicTimer
    participant M as DeviceMonitorService
    participant C as OperationCoordinator

    loop 每 3s
        T->>M: 心跳刷新
        Note over M: coordinator.IsBusy → 跳过
        M->>M: 身份变化才 fire DeviceRefreshed
    end
    U->>C: RunAsync(刷写)
    C->>C: SemaphoreSlim 串行化
    Note over M: 操作期间心跳跳过(IsBusy)
    C->>M: StateChanged → 操作结束触发补偿刷新
```

---

## 7. 关键设计决策与踩坑

| 决策 / 坑 | 解决 |
| --- | --- |
| fastboot DLL 错误码不可读(-4/-8),无刷写进度 | 整体移除 DLL,统一 `platform-tools/fastboot.exe`(35.0.2-eng)CLI:可读错误 + GetProcessIoCounters 连续进度 |
| 大分区刷写被固定超时强杀 | flash 用**无进展超时**(`GetProcessIoCounters` 读写仍在就重置);payload_dumper 同理 |
| bezzad/Downloader RangeHigh 只下 1 字节 | 先探测大小设 `RangeHigh = 大小-1`;失败不再假成功(检查 `DownloadFileCompleted` 异常) |
| 下载失败继续跑 PrepareFlash 空路径报错 | `sourcePath` 非空才 `PrepareFlashAsync` |
| staging / 临时 gzip 泄漏数 GB | 失败/取消/退出全链路清理;AppComposition.StopAsync 兜底扫盘 |
| 心跳导致分区表反复重读打断操作 | `DeviceRefreshed` 只在身份变化 / 手动 / 补偿时触发;分区表仅点按读取 |
| 自动刷新静默吞异常留幻影设备 | 连续 3 次失败降级未连接 + 记错误 |
| 备份无完整性校验 | 备份文件长度必须等于 `SizeBytes` |
| Root/快速刷写无串号绑定 | `expectedSerial` 与当前设备比对,防错刷 |
| KernelSU.apk 无校验 | SHA-256 白名单 + AndroidManifest 检查 |
| 取消不杀子进程 | adb / fastboot / scrcpy 全部杀进程树 |
| adb server 版本不一致检测不到设备 | 应用统一用内置 `platform-tools/adb.exe` 起 server |
| 自动投屏开关关闭不取消 | 关闭即取消在途协调 + 停止镜像 |
| 修补输出已存在阻塞重试 | `File.Exists` → 先删再解 |
| tar base-256 长度解析 | `ParseOctal` 支持 base-256 |
| 登录后程序消失 | `ShowDialog` 关闭触发 `OnLastWindowClose` → 改显式主窗 `Closed += Shutdown()` |
| bezzad 多分片缓冲无界致 OOM | `MaximumMemoryBufferBytes=256MB`(库对 ≤0 视为无上限);网速快于磁盘时队列不再堆积至包大小量级 |
| 高速下载进度事件灌爆 UI 线程 | `OtaDownloadService` 进度上报 ~100ms 节流(下载完成事件必达,不丢 100%) |
| scrcpy v4.0 移除 `--adb-path` 秒退 | 启动改为注入 **`ADB` 环境变量** 指向内置 adb(全版本兼容) |
| VIVO 刷写 / ROOT 必须 fastbootd | 快速刷写删除 Fastboot/bootloader 选择恒走 fastbootd;ROOT 自动流程 `adb reboot fastboot` + 等 `is-userspace=yes` 设备 |
| 登出打断运行中的刷写 | 登出按钮 `CanExecute = !Coordinator.IsBusy` + `StateChanged` 重评估,忙时禁用(与 force-exit「刷写中先取消等 Idle 再退」一致) |
| 进度条分散在各页面底部 | 全部主进度统一到右上角「操作进度」区,按操作运行显示(固件提取 / 文件传输入位,页面移除重复条) |
| 传出文件固定下到当前目录 | 弹保存对话框选择位置(选择器可注入以便单测),成功后本地目录跟随 |
| 左侧菜单顺序混乱 | 按刷机链路分组重排(概览/文件/投屏 ‖ 刷写/提取/ROOT ‖ 在线/软件) |

---

## 8. 测试

- **VivoKsu.App.Tests**:约 50 个测试文件、**351 个用例**全绿 —— 覆盖各服务与 VM 的分支、取消、进度、错误路径。
- 关键测试:SafeFlash ADB→fastboot 过渡、本地 gzip 不被误删、截断备份被拒、多布局重解析、单预设只刷单个分区、篡改 APK 被拒、RecordRunner 3 参签名适配、心跳(周期 / force_exit 触发 / goodbye / 瞬时失败恢复 / 426)、在线列表解析与时长;2026-08-15 新增保存对话框注入下载到指定路径、`DownloadToFileAsync` 路径安全校验、登出命令触发回调、**登出忙时禁用**、本地目录跟随。
- 运行:`dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`

---

## 9. 发布与内置组件

```powershell
./scripts/Publish-Release.ps1
```

产出 **self-contained win-x64**:`artifacts/release/VivoKsu-win-x64/` + `.zip` + `.sha256` + `SHA256SUMS.txt`(目录内逐文件清单)。发布前 `Ensure-Scrcpy.ps1` 自动补齐 scrcpy 并清理废弃资源(Sukisu.APK、ksud)。

| 内置组件 | 来源 | 用途 |
| --- | --- | --- |
| `payload-tools/payload_dumper.exe` | payload-dumper-rust | OTA payload 解包、云提取 |
| `platform-tools/adb.exe · fastboot.exe` | Android SDK Platform Tools | 设备通信(回退路径) |
| `platform-tools/fastboot.exe` | fastboot 35.0.2-eng(带进度,用户提供) | 全部 fastboot 刷写 / 读变量 / 擦除 / 重启 / 槽位 |
| `scrcpy/` | scrcpy | 屏幕镜像(发布时自动获取) |
| `root-tools/magiskboot.so` | Magisk | vendor_boot 补丁处理 |
| `apk/KSU.APK · KernelSU.apk` | KernelSU | Root 管理器安装包(带 SHA-256 校验) |

---

## 10. 已知限制

- **payload 分区内部百分比无法测量**:payload_dumper 预分配输出文件且不流式输出进度,分区内进度按进程写入字节驱动(真实但以分区为单位)。
- **分区操作有真实设备风险**:写入 / 擦除修改设备分区,执行前有确认弹窗,任务在首个失败分区停止。
- **`.ps1` 脚本必须纯 ASCII**:本机无 BOM 的 UTF-8 被按 GBK 读取会乱码(中文注释请用英文)。
- **唯一 fastboot.exe 待真机验证**:fastboot 35.0.2-eng 在 vivo fastbootd 逐个刷分区是唯一未真机实测环节。
- **下载盘需 ~25GB 空闲且最好是 SSD**:bezzad 多分片随机写 HDD 会停滞(staging 自动优先系统盘)。
- **VOTA 链接有时效**:`url` 带 `sign`/`t`,拿到后尽快下载。
- **版本门禁**:客户端版本低于后台「Nwflash 版本控制」最低版本 → 服务端 426 强制更新,桌面端弹更新窗。

---

## 相关文档

- [index.md](index.md) —— **项目索引**(所有文档 / 代码 / 服务 / 数据的导航地图)。
- [safeflash-ota.md](safeflash-ota.md) —— 安全刷写流程、OTA 格式、下载/刷写内部细节与踩坑。
- [cloudflare/API.md](../cloudflare/API.md) —— **api.nwflash.cc.cd 接口契约**(端点 / 参数 / 错误码 / 计费 / 功能记录)。
- [cloudflare/README.md](../cloudflare/README.md) —— Cloudflare Worker 部署说明。
- [cloudflare/web/README.md](../cloudflare/web/README.md) —— **web.nwflash.cc.cd 后台管理**。
- [cloudflare/user/README.md](../cloudflare/user/README.md) —— **user.nwflash.cc.cd 用户门户**。
- [cloudflare/website/README.md](../cloudflare/website/README.md) —— **nwflash.cc.cd 官网**(液态玻璃落地页)。
- [README.md](../README.md) —— 项目总览与快速上手。
