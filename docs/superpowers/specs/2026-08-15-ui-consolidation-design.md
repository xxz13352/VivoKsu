# 奶娃Flash 界面整合设计(2026-08-15)

> 状态:**已确认**。用户批准了本设计(含三个决策点),并授权后续自行推进,无需再逐项确认。

## 一、背景与目标

桌面端(WPF,`src/VivoKsu.App`)5 项 UI/交互改动,一次交付:

1. **文件管理「传出文件到电脑」弹保存位置对话框** —— 现在直接下载到 `CurrentLocalPath`(默认桌面)。
2. **固件提取进度条移到右上角刷写位置** —— 固件提取页底部的双进度条搬进右侧 DEVICE STATUS 卡片的进度区。
3. **左下角加账号 id + 当前时间 + 登出按键** —— 左导航栏底部新增固定栏。
4. **全部进度统一到右上角进度区** —— 右上角成为唯一的主进度显示区。
5. **左侧菜单重新排列** —— 按刷机链路分组。

约束:现有 339 个单元测试全绿、所有 ViewModel 可观测属性不变(进度面板是纯 XAML 改动);不引入第三方库。

## 二、决策记录(用户已确认)

| 决策点 | 选择 | 说明 |
| --- | --- | --- |
| 菜单顺序 | **按刷机链路分组** | 设备概览/文件管理/ADB 投屏 ‖ 快速刷写/可视刷写/VIVO 线刷/固件提取/Vivo ROOT ‖ 在线状态/软件 |
| 登出方式 | **同进程回登录窗** | 优雅下线(心跳 goodbye + 上传日志 + 停监视)→ 同一进程内回到登录窗口,无重启闪烁 |
| 进度范围 | **只搬主进度条** | 固件提取底部双进度条、文件管理忙进度条移到右上角;保留可视刷写分区列表行级小进度条 + 「读取分区表」加载条 |

## 三、逐项设计

### 3.1 文件管理「传出文件到电脑」→ 保存对话框

**现状**:`FileManagerViewModel.DownloadAsync`(ViewModels/FileManagerViewModel.cs:290)直接把 `SelectedRemote` 下载到 `CurrentLocalPath`。

**改动**:
- `DownloadAsync` 执行前弹 `SaveFileDialog`:
  - `FileName` = 设备文件名,`InitialDirectory` = `CurrentLocalPath`,`Filter` = `所有文件 (*.*)|*.*`,`Title` = 「选择保存位置」。
  - 取消 → 直接返回(不下载);确认 → 下载到所选完整路径。
- **保存位置选择器做成可注入委托**(默认弹真实对话框),使 `DownloadAsync` 可单测:
  - ctor 新增可选参数 `Func<string initialDir, string defaultName, string? chosenPath>? saveLocationPicker = null`。
- `AdbFileService` 新增 `DownloadToFileAsync(serial, remoteFile, destinationFilePath, ct, context)`:
  - 接收完整目标路径;沿用现有安全校验(非法文件名/保留名/越界抛错)。
  - 复用 `BuildSafeLocalDestination` 逻辑,但基于显式文件名构造目标。
- 下载成功后 `CurrentLocalPath = Path.GetDirectoryName(destination)` 并 `RefreshLocal()`,工作台跟随所选目录。

**测试**:注入假选择器 → 断言下载到所选路径、本地目录跟随;`AdbFileService.DownloadToFileAsync` 的安全校验测试(非法名/保留名)。

### 3.2 + 3.4 右上角统一「操作进度」区

**现状**:右侧列(Column 2,DEVICE STATUS 卡片)内按 `SelectedPage` 显示三组进度:快速刷写(L1502-1528)/可视刷写(L1475-1501)/VIVO 线刷(L1448-1474)。各绑定对应页面 VM 的属性。

**改动**:把这三组改为**按「操作是否在运行」显示**(切换页面进度不消失),并新增两组 + 一个占位:

| 块 | 绑定源(属性均已存在) | 显示条件 |
| --- | --- | --- |
| 快速刷写 | `QuickFlash.CurrentPartition` / `CurrentPartitionProgress` / `CurrentPartitionProgressPercent` / `OverallProgress` / `OverallProgressPercent` / `SpeedText` / `IsCurrentPartitionIndeterminate` | `QuickFlash.IsFlashOperationActive` |
| 可视刷写 | `PartitionWorkspace.CurrentOperationPartitionName` / `CurrentOperationProgress` / `OperationProgressPercent` / `OverallProgress` / `OperationSpeedText` / `IsCurrentOperationIndeterminate` | `PartitionWorkspace.IsExecuting` |
| VIVO 线刷 | `SafeFlash.CurrentPartition` / `CurrentPartitionProgress` / `CurrentPartitionProgressPercent` / `OverallProgress` / `OverallProgressPercent` / `SpeedText` / `IsCurrentPartitionIndeterminate` | `SafeFlash.IsBusy` |
| **固件提取(新增)** | `FirmwareExtract.CurrentPartitionName` / `CurrentPartitionProgress` / `IsCurrentPartitionIndeterminate` / `PayloadProgress` / `PayloadProgressPercent` / `SpeedText` / `ElapsedText` | `FirmwareExtract.IsPayloadBusy` |
| **设备操作(新增,通用)** | `DeviceSession.StatusText`(阶段文案)+ 不定进度条 | `DeviceSession.IsBusy` 且上面四块均非忙 |
| 占位「无进行中的操作」 | — | 全部空闲 |

- 每块格式与现有保持一致:当前分区名 + 当前条 + 百分比/速度 + 总进度条 + 总百分比。
- 固件提取块显示「当前分区(下载固件/解析固件/分区名)+ 速度 · 耗时」与「总进度 + 百分比」,复用 `SpeedText`/`ElapsedText`。
- **设备操作通用块**覆盖文件传输/ROOT/安装 APK/镜像读取等只经协调器上报阶段的操作(`DeviceSession.IsBusy` 由 `OperationCoordinator.RunAsync` 统一置位,见 Services/OperationCoordinator.cs:110)。用 MultiDataTrigger 排除四块特定页面忙(避免闪刷时双显示)。

**页面侧移除(移动,不重复)**:
- 固件提取页底部面板(Index:982-1007)的 `当前分区` + `总进度` 两个 ProgressBar(Column 1 的 StackPanel)——保留状态文案(Column 0)与操作按钮(Column 2)。
- 文件管理页行 4(Index:685)底部 `ProgressBar`(绑定 `DeviceSession.IsBusy`)——保留状态文案。

**保留(用户确认)**:可视刷写分区列表行级 `ProgressBar`(Index:246)与「读取分区表」加载条(Index:304)。

### 3.3 左下角:账号 + 当前时间 + 登出

**左导航栏结构**:`Border Grid.Row="1" Grid.Column="0"`(RailBrush)内的 Grid 从单行改为两行:`*`(导航 StackPanel)+ `Auto`(底部账号栏)。

底部账号栏(160px 宽,紧凑排版,文案截断):
- **账号 id**:登录账号(`AppComposition.CurrentUsername`,启动时注入的是 `login.Username`)。
- **当前时间**:每秒刷新,格式 `MM-dd HH:mm:ss`(小号 Cascadia Mono)。
- **登出**按钮:全宽小按钮,触发 `MainViewModel.LogoutCommand`。

**ViewModel 改动**:
- `MainViewModel` 新增:
  - `[ObservableProperty] string accountName = ""` —— 由 `AppComposition.StartSessionAsync` 在登录后设置。
  - `[ObservableProperty] string currentTimeText = ""` —— 由 `DispatcherTimer`(1s)驱动;`Application.Current is null`(纯单测)时跳过计时器。
  - `LogoutCommand`(AsyncRelayCommand)—— ctor 新增可选 `Func<Task>? onLogout = null`;命令调用 `onLogout`。
- `AppComposition` 新增:
  - `StartSessionAsync` 里设置 `MainViewModel.AccountName = username`。
  - 私有 `OnLogoutAsync()` = `await StopAsync(); LogoutRequested?.Invoke(...)`;构造 MainViewModel 时注入 `onLogout: OnLogoutAsync`。
  - `public event EventHandler? LogoutRequested`。

**App 生命周期(同进程登出)**:
- `App.xaml.cs` `OnStartup`:设置 `ShutdownMode = ShutdownMode.OnExplicitShutdown`(默认 OnLastWindowClose 会随关窗退出)。
- 登录 → 建 composition → 显示主窗;`MainWindow.Closed` 处理器:登出中(`isLogout`)→ 重新进入登录循环;否则 `Shutdown()`。
- `composition.LogoutRequested += (_, _) => { isLogout = true; MainWindow?.Close(); }`。
- 登录循环抽取为 `RunApplicationLoop()`:登录窗成功 → 建新 composition + `StartSessionAsync` + 新 MainWindow + 驱动提醒;取消/失败 → `Shutdown()`。
- `OnExit` 不变:composition 未停则 `StopAsync`(5s DispatcherFrame);登出后已停,幂等守卫(`stopped`)直接返回。

> 登出后旧 composition 已 `StopAsync`(心跳 goodbye、使用日志 flush、Monitor/Coordinator 释放、临时文件清理),新 composition 全新构造,无状态残留。心跳 `StopAsync` 不被旧 composition 的异步续延重入(参考现有 force-exit 死锁防护)。

**测试**:`MainViewModel` 登出命令触发注入回调;`AccountName` 属性设置;计时器在无 `Application.Current` 时不启动。

### 3.5 左侧菜单重排

仅重排 `MainWindow.xaml` 左导航按钮顺序,各按钮的 SelectedPage DataTrigger 保留:

```
设备概览 · 文件管理 · ADB 投屏
──────────
快速刷写 · 可视刷写 · VIVO 线刷 · 固件提取 · Vivo ROOT
──────────
在线状态 · 软件
```

## 四、涉及文件

| 文件 | 改动 |
| --- | --- |
| `src/VivoKsu.App/Services/AdbFileService.cs` | 新增 `DownloadToFileAsync`(完整目标路径 + 安全校验) |
| `src/VivoKsu.App/ViewModels/FileManagerViewModel.cs` | `DownloadAsync` 弹保存对话框(可注入)、成功后本地目录跟随 |
| `src/VivoKsu.App/ViewModels/MainViewModel.cs` | `AccountName`、`CurrentTimeText`(+1s 计时器)、`LogoutCommand`、`onLogout` 注入 |
| `src/VivoKsu.App/Services/AppComposition.cs` | `LogoutRequested` 事件、`OnLogoutAsync`、`StartSessionAsync` 设账号 |
| `src/VivoKsu.App/App.xaml.cs` | `OnExplicitShutdown`、`RunApplicationLoop()`、`LogoutRequested` 接线 |
| `src/VivoKsu.App/MainWindow.xaml` | 左导航重排 + 底部账号栏;右上进度区重构(5 块 + 占位);固件提取/文件管理页内进度条移除 |
| `src/VivoKsu.App/MainWindow.xaml.cs` | (如需要)计时器停止、登出回登录的 Closed 处理 |
| `tests/VivoKsu.App.Tests/` | 新增测试(保存对话框注入、DownloadToFileAsync 校验、LogoutCommand、AccountName) |

## 五、测试与验收

- 现有 339 测试必须全绿(进度区为纯 XAML 改动;VM 属性均保留)。
- 新增测试:
  - `FileManagerViewModel.DownloadAsync` 用注入选择器下载到指定路径 + `CurrentLocalPath` 跟随 + 取消不下载。
  - `AdbFileService.DownloadToFileAsync` 安全校验(非法名/保留名/越界)。
  - `MainViewModel.LogoutCommand` 触发注入回调。
- 手动验收(真实设备不可用则 UI 自动化脚本):登出回登录窗、固件提取进度显示于右上角、左下角时间走动。

## 六、范围外(明确不做)

- 可视刷写分区行级进度条与「读取分区表」加载条保留原位。
- 不引入新的进度聚合 ViewModel(固件提取块直接绑定既有属性;通用块绑定 `DeviceSession`)。若实施中发现 5 块并存的 MultiDataTrigger 过于笨拙,可改用一个极薄的计算属性 `ProgressAreaKind`,但**不改任何现有 VM 属性**。
