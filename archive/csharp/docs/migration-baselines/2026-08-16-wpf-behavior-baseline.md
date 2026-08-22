# 2026-08-16 WPF 行为基线（NWF / 奶蛙Flash）

> 用途：给 5.3codex 迁移到 Tauri 时使用的“可执行对照基线”。
> 范围：仅 WPF 桌面端行为，不修改 `cloudflare/**` 与 Web 部分。
> 版本戳：`2026-08-16`

## 全局边界（必须保持）

- 应用名显示为 **奶蛙Flash**，技术名可继续用 `NWF` / `NWflash`。
- 不改 `cloudflare/**`；后端行为只读映射。
- UI 不大改：当前 11 个主页面与视觉结构保持（见“页面与导航”）。
- 无线改造：token 不持久化存储在前端，登录态仅在会话内存在于进程内。
- 不新增外部库或新命名页，保持 WPF 的操作语义与提示文案可对照迁移。
- 登录、更新、资源、驱动、进度、日志、退出保护是四个“强约束流”，不能弱化。

## 1) 应用启动与生命周期（优先级最高）

- 启动顺序（`App.xaml.cs`）
  1. 注册 `DispatcherUnhandledException` 与 `AppDomain.CurrentDomain.UnhandledException`，异常落盘到 `%LOCALAPPDATA%\VivoKsu\crash.log`。
  2. `BlockForForcedUpdate()` 调用 `/api/app/version`，`force_update=true` 时直接弹更新窗并退出；网络错误不阻断启动。
  3. `ShutdownMode = OnExplicitShutdown`，进入登录主循环 `RunApplicationLoop()`。

- 登录循环（`RunApplicationLoop`）
  1. 弹登录窗 (`LoginWindow`)；取消直接 `Shutdown`。
  2. 登录成功后创建 `AppComposition`。
  3. `StartSessionAsync(token, username)`：注入 token、生成会话 id、启动心跳与在线列表轮询。
  4. 显示主窗 `MainWindow`；监听关闭事件：登出时回到登录窗，不退出进程；其他关闭直接退出。

- 主窗生命周期（`MainWindow` + `AppComposition.StopAsync`）
  - 主流程结束（退出/登出）时：先停止心跳并发送 goodbye，停设备监控，停镜像服务，再 flush 使用日志，最后清理临时文件。
  - 强制下线路径：先 `Heartbeat.SendGoodbyeAsync()`，再 `Coordinator.CancelCurrent()`，空闲后 `Environment.Exit(0)`。
  - 更新窗口弹出后均为无跳过退出。

- 版本与更新行为
  - 任何 426 都为 `UpdateRequiredException`，必须弹强制更新窗（标题/版本/下载链接）并退场。
  - 否则不能给“稍后再说”的绕过路径。

## 2) 页面与导航（11 个页面，含日志区域）

`AppPage` 枚举顺序：
`Overview, AdbActions, FileTransfer, FastbootFlash, LineFlash, FirmwareExtract, OperationLog, RootTools, SafeFlash, OnlineStatus, Software`

实际 WPF 侧边栏显示顺序（保留到 Tauri）：
1. 设备概览
2. 文件管理
3. ADB 投屏
4. 快速刷写
5. 可视刷写
6. VIVO 线刷
7. 固件提取
8. Vivo ROOT
9. 在线状态
10. 软件

说明：**操作日志不在侧边栏做独立导航按钮**，当前在右侧详情面板（`PARTITION / WORKSPACE` 区域右侧卡片中常驻显示）。

## 3) 窗口与布局约束

- 无边框窗口：`WindowStyle="None"`, `ResizeMode=CanResize`，支持拖动标题栏实现。
- 初始窗口：`1240x700`，最小 `1120x620`。
- 主布局列：`160 / * / 286`（左侧导航 + 主内容 + 右侧状态区）。
- 右侧状态区内容固定包括：
  - 当前账号（`AccountName`）
  - 时间（`MM-dd HH:mm:ss`）
  - 统一进度文本（空闲/设备操作）
  - 退出按钮
- 左右边界/间距/色系按现有 WPF 样式文件（资源字典）映射，不允许改造为新框架视觉。

## 4) 统一进度与状态优先级（关键对照）

WPF 进度条区域显示顺序为优先级高低：
1. `PartitionWorkspace.IsRefreshing`
2. `QuickFlash.IsFlashOperationActive`
3. `LineFlash.IsActive`
4. `SafeFlash.IsBusy`
5. `FirmwareExtract.IsPayloadBusy`
6. 设备通用操作（`DeviceSession.IsBusy`）及以上均空闲。

空闲文本为：“无进行中的操作”。

## 5) 登出行为

- `MainViewModel.CanLogout`：仅当 `OperationCoordinator.IsBusy == false` 且 `DeviceSession.IsBusy == false` 时可执行。
- 登出按钮在后台操作进行中要禁用，避免打断刷写/传输。

## 6) 操作日志（右侧面板）

- 日志内存上限：500 条（超出即移除最旧）。
- 显示格式：`[HH:mm:ss] 消息` + 级别着色（Success/Warning/Error）。
- 条目来源可来自设备服务、分区刷写、ROOT、下载、镜像操作等。
- “清空”只清空内存列表，不清空磁盘持久化日志文件。

## 7) 全局服务行为（对接 Rust 时必须一致）

### 身份与版本
- `X-Nwflash-Version` 头在每次 Cloudflare 请求都要加。
- 登录成功返回 token 后仅在进程内持有，`OtaApiClient.Token` 仅设置 `Authorization: Bearer <token>`，不写本地。

### 登录与 token 验证
- `/api/login` 与 `/api/me` 只读失败时都不吞错：401/400/426 要按语义反馈。
- `/api/login` 使用账号 + 密码必填；空登录框禁止提交。

### 会话心跳（`HeartbeatService`）
- 周期：`5s`，请求超时：`10s`，goodbye 超时：`3s`。
- 强制退出原因：`/api/heartbeat` 返回 `force_exit=true` 或状态 401/403，触发服务端下线流程。
- 426 进入更新流；网络抖动不阻塞业务，只重试。

### 分布式操作
- `OperationCoordinator` 仅允许单一并发。
- 任一新操作在已有进行中时立即报 `已有任务正在进行中，请等待其完成或先取消。`，不排队。
- 操作门禁失败（server deny）不进入实际执行；权限拒绝要可见提示并保留日志。
- 进度仅节流到 100ms 写入状态，避免 UI 过载。

## 8) 关键模态窗（必须保留行为）

- 登录窗：
  - 账号 + 密码必填；
  - 登录中展示遮罩与 spinner；
  - 更新要求直接透传更新窗；
  - ESC/关闭可退出登录弹窗。
- 更新窗（UpdateRequiredWindow）：
  - 展示 latest/min/version/download（如有）；
  - 没有“下次再说”分支。
- 资源下载窗：
  - 组件缺失时由登录后检测触发；
  - `安装 / 取消 / 关闭` 可控，取消时应中断下载请求。
- 驱动提醒窗：
  - 缺少 ADB/Fastboot 时弹出；
  - 安装中锁定关闭（避免中断）。

## 9) 关键页面基线（高优先）

- **设备概览**：显示设备连接状态、连接文本、刷机建议入口。
- **文件管理**：ADB 文件列表 + 上传下载 + 删除，支持取消时不中断主线程。
- **ADB 投屏**：打开 scrcpy 后可重连，异常自动恢复有次数限制。
- **快速刷写**：镜像选取、镜像预检、预置清单、刷写确认、速度/进度/速度和日志跟踪。
- **可视刷写**：需手动读取分区表，支持三种传输通道（auto/adb-root/fastboot），分区筛选与批量提交，失败则停止。
- **VIVO 线刷**：支持 OTA 解包与 Fastboot 刷入链路，含 clear data 选项；刷入过程可中断且可重试。
- **固件提取**：按 PD + 版本解析 payload -> 提取/映射到快速刷写预设；映射后跳转快速刷写页。
- **Vivo ROOT**：资源/补丁校验、镜像修补产物生成后可转快速刷写。
- **在线状态**：读取 `/api/online`，显示会话名/版本/时长，轮询 5s（默认）。
- **软件**：显示 scrcpy/驱动/payload 组件状态；支持重装入口（无则引导下载）。

## 10) 可直接复用的测试锚点（给 5.3codx）

- `tests/VivoKsu.App.Tests/OtaApiClientTests.cs`
- `tests/VivoKsu.App.Tests/HeartbeatServiceTests.cs`
- `tests/VivoKsu.App.Tests/OperationCoordinatorTests.cs`
- `tests/VivoKsu.App.Tests/AppVersionControlTests.cs`
- `tests/VivoKsu.App.Tests/OperationLogServiceTests.cs`
- `tests/VivoKsu.App.Tests/MainViewModelTests.cs`
- `tests/VivoKsu.App.Tests/DeviceMonitorServiceTests.cs`
- `src/VivoKsu.App/App.xaml.cs`, `src/VivoKsu.App/MainWindow.xaml`（作为视觉和生命周期金标准）

## 11) 执行建议给 5.3codx

1. 先不写代码前，把本文件与 `api-contract-cases.md` 作为唯一迁移规则源。
2. 页面壳体先做“只读展示版本”，再逐页加服务与命令，保持文案/顺序/禁用态逐条比对。
3. 任一阶段若出现差异，优先回退到本基线再补齐，不要先追求功能完整再补行为。

## 12) Tauri 截图与视觉对照状态

> 本节按时间保留迁移过程中的阶段记录；其中出现的“未完成”“阻断”或“不得启动”均为当时状态。当前有效结论以本文件最后的“2026-08-17 Task 16 Fix Round 2 闭环”为准，Task 16 已完成并可进入 Task 17。

- 2026-08-16 已由浏览器模式的 Tauri WebdriverIO 验收生成
  `screenshots/tauri-software-ready.png`。该截图覆盖“软件”页的已就绪状态。
  软件状态所需 command DTO 由测试夹具提供；右侧常驻日志面板的初始调用早于
  浏览器模式 mock 注册，可能包含该测试运行时的未 mock 诊断。
- 当前已补充由 `tests/VivoKsu.VisualCapture` 生成的 11 张 WPF 主窗空闲态基线：
  `wpf-overview-idle.png`、`wpf-adbactions-idle.png`、`wpf-filetransfer-idle.png`、
  `wpf-fastbootflash-idle.png`、`wpf-lineflash-idle.png`、`wpf-firmwareextract-idle.png`、
  `wpf-operationlog-idle.png`、`wpf-roottools-idle.png`、`wpf-safeflash-idle.png`、
  `wpf-onlinestatus-idle.png` 与 `wpf-software-idle.png`。采集器只调用公开的
  `AppComposition.CreateForTesting`，使用空设备/空进程实现，不请求登录、Cloudflare 或设备；每张
  PNG 都在真实 WPF `MainWindow` 视觉树完成布局后以 `1240x700` 输出。
- 这些新 WPF 图目前尚未全部有相同状态、相同视口的 Tauri 配对图，不能据此标记“等价”或
  “允许的框架差异”。
- 在补齐每个 `<page>-<state>.png` 的同名 WPF/Tauri 截图并逐项分类之前，此项保持为待比较，
  不作为任务 16 的视觉通过证据。

### 2026-08-17 登录空闲态对照

- 新增 `screenshots/tauri-login.png`：嵌入式 Tauri E2E 在允许版本、无 token 的启动 fixture 下生成，
  客户区为 `400x564`，登录卡为 `352x516`，并使用 WPF 的品牌图、关闭控件和密码眼睛字形。
- 对照 `screenshots/wpf-login.png`，该登录窗结构、尺寸、字段顺序、按钮文案和品牌资产分类为
  **允许的框架差异**：WPF 的透明无边框宿主显示桌面背景，Tauri WebView 宿主显示其浅色客户区背景；
  两者不影响登录门禁或可操作内容。
- 其余十个主页面仍缺对应 WPF 同状态截图和分类，故 Task 16 的全页面视觉认证仍为**阻断差异**，
  不得据此启动 Task 17。

### 2026-08-17 主窗外壳对照修复（未完成）

- Tauri 的浏览器视觉用例现在直接断言 WPF 外壳资源色：窗口 `#F7F9FB`、导航和右侧表面
  `#FFFFFF`、中心画布 `#F8FAFC`、边线 `#DDE5EA`、选中导航 `#E8F4F2/#087A70`。该断言由真实
  DOM 计算样式而非源文本检查。
- 已修复的范围仅为全局 token、主窗四个表面、导航通用态、通用按钮和日志表面；其命令、DTO、
  模态与授权边界没有变化。
- `tauri-software-ready.png` 仍与 `wpf-software-idle.png` 存在**阻断差异**：Tauri 页面仍渲染迁移期的
  版本/API 说明和简化列表，WPF 为分组组件状态表，且二者尚未在同一 `1240x700` 原生客户端视口采集。
  因此 Task 16 仍未通过。

### 2026-08-17 Software 原生空闲态复测（未完成）

- 已由嵌入式 Tauri WDIO 在真实 `1240x700` 客户区重新生成
  `screenshots/tauri-software-idle.png`。该用例在登录前注册空日志快照，故右侧为与 WPF 相同的
  零条目 `ACTIVITY LOG / SESSION LOG` 空态，而非测试运行时日志。
- 原生回归断言已验证 Software 页头 `y=94`、组件表 `y=188..564`（高度 `376`）和页面文档高度
  `700`，消除了此前的额外顶部间距、表格行高偏差与文档级滚动条。组件文案已直接回归 WPF 的
  Fastbootd、scrcpy 和 payload 状态文本。
- WPF 采集器现在以 `VivoKsu.App;component` 限定品牌 pack URI，并在任何视觉树 `Image` 未得到
  非零像素尺寸时拒绝输出。已用该采集器重新生成全部 11 张 WPF 空闲态图；这修复了旧基线品牌图
  未解码的问题，未改变页面或业务行为。
- Software 这一对当前分类为**允许的框架差异**：同源品牌图、`1240x700` 客户区、主三栏、状态组
  文案、表格边界和零条目日志轨道一致；残余为 WPF 与 WebView 的字体栅格、按钮字形和窗口控制
  字形差异，不改变内容、顺序或操作。其余十个 `AppPage` 仍缺同状态 Tauri 原生配对图与分类，故
  Task 16 仍未通过。

### 2026-08-17 Overview 原生空闲态复测（未完成）

- 嵌入式 Tauri WDIO 已在真实 `1240x700` 客户区生成
  `screenshots/tauri-overview-idle.png`，输入为与 `wpf-overview-idle.png` 相同的断开设备快照：
  `未检测到设备 / 等待连接 / --`，且操作日志固定为空。
- 原生规格断言页面标题 `y=94`、设备档案 `y=188` 且高 `272`、启动控制 `y=520` 且高 `96`，以及
  文档高度 `700`。设备档案保留 WPF 的 `DEVICE / OVERVIEW`、六格只读信息、
  `READ-ONLY DEVICE PROFILE` 和三块 `REBOOT CONTROL` 布局；重启仍只调用既有无参数 Rust command。
- Overview 这一对分类为**允许的框架差异**：三栏尺寸、空闲文案、档案和启动控制的结构/坐标一致；
  差异只包括 WPF 与 WebView 的字体栅格、标题栏按钮和窗口控制字形，不改变页面内容或设备操作。
  已完成 Software 与 Overview 两对；其余九个 WPF `AppPage` 仍缺同状态 Tauri 原生配对图和分类，
  因此 Task 16 仍为**阻断差异**，不得启动 Task 17。

### 2026-08-17 File Manager 原生空闲态复测（未完成）

- 嵌入式 Tauri WDIO 已在真实 `1240x700` 客户区生成
  `screenshots/tauri-filetransfer-idle.png`，输入为空 `/sdcard` 目录、零条目、空日志和断开设备状态，
  与 `wpf-filetransfer-idle.png` 的测试替身状态一致。
- 原生规格固定标题 `y=94`、工作台 `y=185`、目录摘要 `y=268`、文件卡片内容区 `340px`；后者加上
  上下各 `10px` 的 WPF 等效边距，保持 XAML 的 `360px` 文件行，并使“文件日志”起始行在首屏可见。
  外壳固定为客户端高度，页面滚动只发生在中心 `nw-page-card`，不会再将 WebView 文档扩展到
  `828px`。
- File Manager 这一对分类为**允许的框架差异**：标题、工具带顺序、目录摘要、空文件区、文件日志和
  ADB 状态脚注与 WPF 一致；图标字形、字体栅格、窗口控制和 disabled 颜色由 WebView 呈现，不改变
  受限上传、下载、APK 安装或删除确认操作。已完成三对 WPF `AppPage`；剩余八对仍阻断 Task 16。

### 2026-08-17 ADB ScreenCast 原生空闲态复测（未完成）

- 嵌入式 Tauri WDIO 已生成 `screenshots/tauri-adbactions-idle.png`，与
  `wpf-adbactions-idle.png` 一样使用断开设备、未启动镜像进程和空操作日志。
- 原生规格固定 `1240x700` 客户区、标题 `y=94`、控制台 `y=188` 且高 `356`，其内部行高直接对应
  WPF `88 / 184 / 82` 的 SCRCPY 会话栏、手动/自动控制区和双状态底栏。页面不再显示 session ID 或
  操作日志摘要；这些信息只留在全局状态轨/日志轨。
- ADB ScreenCast 这一对分类为**允许的框架差异**：标题、选择 scrcpy 操作、连接状态、控制台文案、
  start/stop、自动投屏开关和双状态底栏与 WPF 一致。WebView 的文字栅格、按钮/开关字形和窗口控制
  字形不同，但不会改变 `mirror_start`、`mirror_stop` 或 `mirror_set_auto` 的受限调用。已完成四对，
  剩余七对 WPF `AppPage` 仍阻断 Task 16。

### 2026-08-17 Quick Flash 原生空闲态复测（未完成）

- 嵌入式 Tauri WDIO 已生成 `screenshots/tauri-fastbootflash-idle.png`，与
  `wpf-fastbootflash-idle.png` 一样使用未选择镜像的四个预置槽和断开设备状态。
- 原生规格固定 `1240x700` 客户区、标题 `y=94`、预置面板 `y=188` 且高 `198`。面板保留 WPF 的
  `FLASH / PRESET` 标题、开始刷入、自动重启、等待 FB 设备、双刷入双槽、刷完切槽和
  `boot/init_boot/vendor_boot/lk` 四个镜像槽。
- Quick Flash 这一对分类为**允许的框架差异**：预置首屏布局和交互文案一致；WebView 控件字形和
  disabled 色阶不同，不改变镜像选择、Rust 预检、双槽冻结确认或无参数执行入口。高级分区工作区仍在
  同页下方滚动区域，避免侵占 WPF 空闲首屏。已完成五对，剩余六对 WPF `AppPage` 仍阻断 Task 16。

### 2026-08-17 Line Flash 原生空闲态复测（未完成）

- 嵌入式 Tauri WDIO 已生成 `screenshots/tauri-lineflash-idle.png`，与
  `wpf-lineflash-idle.png` 一样使用断开设备、空分区快照和空日志。
- 原生规格固定 `1240x700` 客户区、标题 `y=94`、分区工作区 `y=184` 且高 `394`、底部任务栏
  `y=592` 且高 `76`。工作区保留 WPF 的 `PARTITION / WORKSPACE` 标题、自动/ADB Root/Fastboot
  通道选择、读取分区表、筛选行、空态与备份/写入/擦除/停止任务栏。
- Line Flash 这一对分类为**允许的框架差异**：通道选择传递的只是 `Automatic`、`AdbRoot`、`Fastboot`
  封闭枚举；Rust 仍由当前设备快照验证模式并持有 serial 和固定命令。WPF 与 WebView 的文字栅格、
  空态图标和窗口控制字形不同，不改变读取、镜像映射、确认后备份/写入/擦除或统一取消行为。已完成六对，
  剩余五对 WPF `AppPage` 仍阻断 Task 16。

### 2026-08-17 VIVO Line Flash 原生空闲态复测（未完成）

- 嵌入式 Tauri WDIO 已生成 `screenshots/tauri-safeflash-idle.png`，与
  `wpf-safeflash-idle.png` 一样使用未连接 ADB 设备、空日志和未开始操作状态。
- 原生规格固定 `1240x700` 客户区、标题 `y=94`、安全刷写工作台 `y=184` 且高 `426`、状态栏
  `y=610` 且高 `58`。工作台保留 WPF 的 `VIVO LINE FLASH` 标题、设备摘要、下载+刷入/选择固件/
  选择解包文件夹、刷写选项、槽位选择、确认提示和停止操作栏。
- VIVO Line Flash 这一对分类为**允许的框架差异**：来源动作仍只调用既有受控 `safe_flash_*` command，
  确认前不执行刷写且执行只消费 Rust 内存中的不透明预检 ID。WPF 与 WebView 的字体、复选框/单选框和
  窗口控制字形不同，不改变授权、协调器取消、设备模式校验或 staging 清理规则。已完成七对，
  剩余四对 WPF `AppPage` 仍阻断 Task 16。

### 2026-08-17 固件、ROOT、在线与日志轨空闲态认证

- `tauri-firmwareextract-idle.png` 与 `wpf-firmwareextract-idle.png` 均为未读取 payload、零分区和空日志。
  原生规格固定标题 `y=94`、payload 工作台 `y=184..580`（高 `396`）和状态栏 `y=594..668`（高 `74`）。
  本段旧的阻断结论已由 2026-08-17 Task 16 Fix Round 2 取代：Tauri 现将固件源文件选择和输出目录选择分为两个动作；输出目录 picker 只保存目的地，绝不调用固件检查。
- `tauri-roottools-idle.png` 与 `wpf-roottools-idle.png` 均为未选择启动镜像的 Vivo KSU 状态。原生规格固定
  标题 `y=94`、ROOT 工作台 `y=182`、高 `486`。本段旧的阻断结论已由 2026-08-17 Task 16 Fix Round 2 取代：Tauri 现在由单个 Rust `root_run_automatic` coordinator transaction 实现 WPF 自动 ROOT 路径。
- `tauri-onlinestatus-idle.png` 与 `wpf-onlinestatus-idle.png` 均为零在线会话和空日志。原生规格固定标题
  `y=94`、会话工作台 `y=184`、高 `484`；两图分类为**允许的框架差异**，残余仅为 WebView 字体/控件字形，
  不改变首次读取、5 秒轮询或 `online_sessions` 的只读调用。
- `tauri-operationlog-idle.png` 现在是 OperationLog 自己生成的原生证据，显示 `ACTIVITY LOG`、`SESSION LOG`
  和零条目状态。旧的“缺少空白中心工作区”阻断结论已由 2026-08-17 Task 16 Fix Round 2 取代：`OperationLogPage` 保持空中心，而日志始终位于右侧轨道。WPF 的 `MainWindow.xaml` 只提供十个可见导航按钮，未为 `AppPage.OperationLog` 提供导航或中心 `DataTrigger`；Tauri 因而同样不增加第十一项侧栏入口。
- 10 个主导航页和登录页均有同状态原生截图；OperationLog 也有独立命名的原生截图。此前列出的 Firmware Extract、Root 与 OperationLog 行为差异均已在 2026-08-17 Task 16 Fix Round 2 关闭。

### 2026-08-17 Task 16 Fix Round 1 UI 自动化证据

- `native-ui-state-matrix.e2e.ts` 在真实 Tauri/WebView2 主窗中声明 10 个中心工作区和常驻 OperationLog
  共 11 个 UI surface，并逐一保存 loading/error/running 截图。loading/error 来自每页均可见的真实
  OperationLog 异步状态；running 来自真实 React `operation:snapshot` listener、统一进度区和日志条目。
  `tauri-operationlog-idle.png` 与 3 张 OperationLog 状态图均由该 surface 自己触发保存。
- 嵌入式原生 WDIO 全量为 7 个 spec file、39 个通过：导航 3、原生空闲视觉 10、11-surface 状态矩阵 5、
  模态 1、进度 1、direct invoke 3、交互 16。direct mock 现在在 WebView 内记录实参，并由 `update()`
  同步到 host；两个取消用例在断言零调用前执行同步，另有正向用例证明调用会进入 ledger。
- 前端全量 Vitest 为 20 个文件、127 个通过；生产 Vite 构建通过；浏览器模式生产视觉基线为 1 个 spec、
  1 个通过。浏览器模式 focus hook 与嵌入式 teardown 仍输出 `@wdio/tauri-service` 1.3.0 的已知非业务警告，
  reporter 均以退出码 0 完成。
- 标题栏原生回调、拖动区域、登录后资源/驱动顺序检查和保留下载 URL 的无跳过更新窗已有自动化证据。
  本段的阻断结论已由下列 Task 16 Fix Round 2 闭环取代。

### 2026-08-17 Task 16 Fix Round 2 闭环

- Firmware Extract 的输出目录操作与 WPF `FirmwareExtractViewModel.SelectOutputPathAsync` 对齐：目录只作为提取目的地保存，固件检查仅由固件源选择触发。
- 自动 ROOT 使用单个 `root_run_automatic` Tauri command 和一次 `OperationCoordinator` 事务。Rust 捕获设备 identity 并绑定管理器、镜像、KMI、修补工件和 fastbootd serial；官方 KernelSU 必须使用当前的 `init_boot` 与 `vendor_boot`，所有选择句柄都是单次消费。
- OperationLog 继续是永久右侧轨道，内部 `AppPage.OperationLog` 只呈现空白中心。这与 WPF 的十个可见导航入口和没有 OperationLog 中心 `DataTrigger` 的实际 XAML 一致，故不新增导航项。
- 独立 scoped review 结论为 `SPEC: PASS`、`QUALITY: PASS`。原生 WDIO 最终 reporter 为 7 个 spec file / 39 个断言通过；浏览器生产视觉基线为 1 个 spec 通过。WDIO browser-mode 窗口查询、embedded mock-store teardown 和磁盘空间 warning 均为第三方已知告警，未改变 reporter 结果或退出码。
