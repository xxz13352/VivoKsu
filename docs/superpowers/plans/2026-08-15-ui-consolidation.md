# 界面整合(保存对话框/进度统一右上角/登出/菜单重排)实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成奶娃Flash 桌面端 5 项界面改动:文件管理传出弹保存对话框、右上角统一操作进度区(含固件提取/文件传输)、左下角账号+时间+登出、左侧菜单按刷机链路重排。

**Architecture:** 纯 WPF 界面 + ViewModel 增强。核心逻辑改动集中在 `AdbFileService`(新增按完整路径下载)、`FileManagerViewModel`(可注入保存位置选择器)、`MainViewModel`(账号/时钟/登出命令)、`AppComposition`(登出优雅下线事件)、`App.xaml.cs`(同进程回登录循环)。右上角进度区是纯 XAML 重构,现有 VM 进度属性全部复用。

**Tech Stack:** .NET 8 `net8.0-windows`、WPF、CommunityToolkit.Mvvm 8.4、HandyControl、xunit + FluentAssertions。

## Global Constraints

- 客户端 UI 显示名统一用 **「奶娃Flash」**,任何 UI 文案禁止出现 "Nwflash"。
- 现有 **339 个测试必须全绿**(完成后只增不减)。进度区改动不得触碰任何现有 VM 可观测属性。
- **不引入任何新第三方包**。
- 所有可注入/可测试的接缝都用可选参数(默认走真实行为),不得破坏现有测试构造方式。
- 提交粒度:每个 Task 独立提交,信息用规范前缀(`feat:` / `refactor:` / `docs:`)。
- 文件路径默认相对仓库根 `C:\Users\17254\Desktop\TOOL\VivoKsu 工具`。

**执行顺序依赖:** Task 1→2(下载链路)→ Task 3→4(登出链路)→ Task 5(App 生命周期)→ Task 6、7(纯 XAML)→ Task 8(全量收尾)。Task 5/6/7 无单测,以 `dotnet build` 通过为准,由 Task 8 统一跑全量测试。

---

### Task 1: `AdbFileService.DownloadToFileAsync`(按完整路径下载)

**Files:**
- Modify: `src/VivoKsu.App/Services/AdbFileService.cs`(在现有 `DownloadAsync` 之后新增方法 + 私有校验助手)
- Test: `tests/VivoKsu.App.Tests/AdbFileServiceTests.cs`

**Interfaces:**
- Consumes: 现有 `backend.PullAsync(string serial, string remotePath, string localPath, CancellationToken)`(FastbootRsBackend),`OperationLogService.Report`。
- Produces: `Task DownloadToFileAsync(string serial, DeviceFileEntry remoteFile, string destinationFilePath, CancellationToken ct, OperationContext? context = null)` —— 供 Task 2 的 `FileManagerViewModel.DownloadAsync` 调用。

- [ ] **Step 1: 写失败测试**(追加到 `AdbFileServiceTests.cs` 的 `FileNativeApi` 中记录 `PullDestination`)

先给 `FileNativeApi` 加记录属性,再把 `Pull` 方法改为记录目标路径:

```csharp
public string? PullDestination { get; private set; }
public long Pull(string? serial, string remotePath, string localPath) { PullCalled = true; PullDestination = localPath; return 0; }
```

新增两个测试:

```csharp
[Fact]
public async Task DownloadToFileAsync_pulls_to_the_exact_destination_path()
{
    var destination = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"), "renamed.bin");
    var native = new FileNativeApi();
    var service = new AdbFileService(new FastbootRsBackend(native), new OperationLogService());
    var remoteFile = new DeviceFileEntry("update.zip", "/sdcard/update.zip", false, 1024);

    await service.DownloadToFileAsync("RF8", remoteFile, destination, CancellationToken.None);

    Assert.True(native.PullCalled);
    Assert.Equal(destination, native.PullDestination);
}

[Theory]
[InlineData("..\\outside.bin")]
[InlineData("C:\\outside.bin")]
[InlineData("bad:name.bin")]
[InlineData("CON.img")]
[InlineData("trailingdot.")]
public async Task DownloadToFileAsync_rejects_unsafe_device_file_names(string remoteName)
{
    var destination = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"), remoteName);
    var service = new AdbFileService(new FastbootRsBackend(new FileNativeApi()), new OperationLogService());
    var remoteFile = new DeviceFileEntry(remoteName, $"/sdcard/{remoteName}", false, 1);

    await Assert.ThrowsAsync<ArgumentException>(() =>
        service.DownloadToFileAsync("RF8", remoteFile, destination, CancellationToken.None));
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~DownloadToFileAsync" -c Debug`
Expected: FAIL(编译失败:`DownloadToFileAsync` 不存在)。

- [ ] **Step 3: 实现**

在 `AdbFileService.cs` 现有 `DownloadAsync` 方法后新增(复用现有 `IsReservedWindowsFileName`):

```csharp
public async Task DownloadToFileAsync(
    string serial,
    DeviceFileEntry remoteFile,
    string destinationFilePath,
    CancellationToken cancellationToken,
    OperationContext? context = null)
{
    var destination = ValidateSafeDestination(destinationFilePath, remoteFile.Name);
    Report(context, OperationLogLevel.Info, $"正在下载 {remoteFile.FullPath}。");
    await backend.PullAsync(serial, remoteFile.FullPath, destination, cancellationToken);
    Report(context, OperationLogLevel.Success, "文件下载完成。");
}
```

把现有 `BuildSafeLocalDestination` 的文件名校验抽成可复用的私有助手(不改它的行为,`DownloadAsync` 继续用它):

```csharp
private static void ValidateSafeFileName(string fileName)
{
    if (string.IsNullOrWhiteSpace(fileName)
        || fileName is "." or ".."
        || fileName.EndsWith(' ')
        || fileName.EndsWith('.')
        || Path.IsPathRooted(fileName)
        || fileName.Any(character => character < ' ' || Path.GetInvalidFileNameChars().Contains(character))
        || IsReservedWindowsFileName(fileName))
    {
        throw new ArgumentException("设备文件名无法安全保存到 Windows。", nameof(fileName));
    }
}

private static string ValidateSafeDestination(string destinationFilePath, string fileName)
{
    ValidateSafeFileName(fileName);
    if (string.IsNullOrWhiteSpace(destinationFilePath))
    {
        throw new ArgumentException("下载目标路径为空。", nameof(destinationFilePath));
    }

    var fullPath = Path.GetFullPath(destinationFilePath);
    if (string.IsNullOrWhiteSpace(Path.GetDirectoryName(fullPath)))
    {
        throw new ArgumentException("下载目标目录无效。", nameof(destinationFilePath));
    }

    return fullPath;
}
```

把现有 `BuildSafeLocalDestination` 改为调用 `ValidateSafeFileName`(保持等价行为):

```csharp
private static string BuildSafeLocalDestination(string localDirectory, string fileName)
{
    ValidateSafeFileName(fileName);
    var normalizedDirectory = Path.GetFullPath(localDirectory);
    var destination = Path.GetFullPath(Path.Combine(normalizedDirectory, fileName));
    var directoryPrefix = Path.EndsInDirectorySeparator(normalizedDirectory)
        ? normalizedDirectory
        : normalizedDirectory + Path.DirectorySeparatorChar;
    if (!destination.StartsWith(directoryPrefix, StringComparison.OrdinalIgnoreCase))
    {
        throw new ArgumentException("下载目标超出当前本地目录。", nameof(fileName));
    }

    return destination;
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~DownloadToFileAsync|FullyQualifiedName~DownloadAsync" -c Debug`
Expected: PASS(新增 2 个 + 原 `DownloadAsync` 系列仍绿)。

- [ ] **Step 5: 提交**

```bash
git add src/VivoKsu.App/Services/AdbFileService.cs tests/VivoKsu.App.Tests/AdbFileServiceTests.cs
git commit -m "feat: AdbFileService 支持按完整目标路径下载(供保存对话框使用)"
```

---

### Task 2: `FileManagerViewModel` 传出文件弹保存对话框

**Files:**
- Modify: `src/VivoKsu.App/ViewModels/FileManagerViewModel.cs`(ctor 增注入参数、`DownloadAsync` 改流程、新增两个私有方法)
- Test: `tests/VivoKsu.App.Tests/FileManagerViewModelTests.cs`

**Interfaces:**
- Consumes: Task 1 的 `AdbFileService.DownloadToFileAsync`;现有 `CurrentLocalPath`、`SelectedRemote`、`RunCoordinatedAsync`、`RefreshLocal`。
- Produces: ctor 新增可选参数 `Func<string initialDir, string defaultName, string? chosenPath>? saveLocationPicker = null`(Task 3 无关,但供测试注入)。

- [ ] **Step 1: 写失败测试**(追加到 `FileManagerViewModelTests.cs`,复用文件里现有 `EmptyNativeApi`)

```csharp
[Fact]
public async Task Download_uses_the_injected_save_location_and_follows_the_chosen_directory()
{
    var session = new DeviceSessionViewModel();
    session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
    var downloadDir = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
    Directory.CreateDirectory(downloadDir);
    var chosen = Path.Combine(downloadDir, "update.zip");
    string? pickerInitialDir = null;
    var viewModel = new FileManagerViewModel(
        session,
        new AdbFileService(new FastbootRsBackend(new EmptyNativeApi()), new OperationLogService()),
        new OperationLogService(),
        saveLocationPicker: (initialDir, defaultName) => { pickerInitialDir = initialDir; return chosen; });
    viewModel.CurrentLocalPath = downloadDir;
    viewModel.SelectedRemote = new DeviceFileEntry("update.zip", "/sdcard/update.zip", false, 1024);

    await viewModel.DownloadCommand.ExecuteAsync(null);

    Assert.Equal(downloadDir, pickerInitialDir);
    Assert.Equal(downloadDir, viewModel.CurrentLocalPath);
    Assert.Equal(OperationKind.Completed, session.OperationKind);
}

[Fact]
public async Task Download_does_not_download_when_the_save_dialog_is_cancelled()
{
    var session = new DeviceSessionViewModel();
    session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "ADB 已连接"));
    var viewModel = new FileManagerViewModel(
        session,
        new AdbFileService(new FastbootRsBackend(new EmptyNativeApi()), new OperationLogService()),
        new OperationLogService(),
        saveLocationPicker: (_, _) => null);
    viewModel.SelectedRemote = new DeviceFileEntry("update.zip", "/sdcard/update.zip", false, 1024);

    await viewModel.DownloadCommand.ExecuteAsync(null);

    Assert.Equal(OperationKind.Idle, session.OperationKind);
    Assert.False(session.IsBusy);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~Download_uses_the_injected|FullyQualifiedName~Download_does_not_download" -c Debug`
Expected: FAIL(编译失败:ctor 没有 `saveLocationPicker` 参数)。

- [ ] **Step 3: 实现**

ctor 加参数字段:

```csharp
private readonly Func<string, string, string?>? saveLocationPicker;

public FileManagerViewModel(
    DeviceSessionViewModel session,
    AdbFileService files,
    OperationLogService logs,
    IOperationCoordinator? coordinator = null,
    Func<string, string, string?>? saveLocationPicker = null)
{
    this.session = session;
    this.files = files;
    this.logs = logs;
    this.coordinator = coordinator;
    this.saveLocationPicker = saveLocationPicker;
    // ...其余不变
}
```

`DownloadAsync` 整体替换(注意 coordinator 与非 coordinator 两分支都要用 `DownloadToFileAsync` 并跟随目录):

```csharp
private async Task DownloadAsync()
{
    if (SelectedRemote is null || session.ConnectionState != DeviceConnectionState.AdbConnected)
    {
        return;
    }

    var selected = SelectedRemote;
    var destination = PickSaveLocation(selected.Name);
    if (destination is null)
    {
        return; // 用户取消保存对话框,不下载。
    }

    if (coordinator is not null)
    {
        await RunCoordinatedAsync(OperationKind.Transferring, $"正在下载 {selected.Name}", async (context, cancellationToken) =>
        {
            await files.DownloadToFileAsync(session.Serial, selected, destination, cancellationToken, context);
            FollowDownloadedLocation(destination);
        });
        return;
    }

    session.BeginOperation(OperationKind.Transferring, $"正在下载 {selected.Name}");
    try
    {
        await files.DownloadToFileAsync(session.Serial, selected, destination, CancellationToken.None);
        session.CompleteOperation("文件下载完成");
        FollowDownloadedLocation(destination);
    }
    catch (Exception exception)
    {
        session.FailOperation("文件下载失败");
        logs.Write(OperationLogLevel.Error, exception.Message);
    }
}

private string? PickSaveLocation(string defaultFileName)
{
    if (saveLocationPicker is not null)
    {
        return saveLocationPicker(CurrentLocalPath, defaultFileName);
    }

    var dialog = new SaveFileDialog
    {
        FileName = defaultFileName,
        InitialDirectory = CurrentLocalPath,
        Filter = "所有文件 (*.*)|*.*",
        Title = "选择保存位置"
    };
    return dialog.ShowDialog() == true ? dialog.FileName : null;
}

private void FollowDownloadedLocation(string destinationFilePath)
{
    var directory = Path.GetDirectoryName(destinationFilePath);
    if (string.IsNullOrWhiteSpace(directory))
    {
        return;
    }

    CurrentLocalPath = directory;
    RefreshLocal();
}
```

> 注意:`DownloadAsync` 里 `selected` 在 await 前已捕获;删除对 `CurrentLocalPath` 的旧下载调用。`Microsoft.Win32` 已在文件头 using。

- [ ] **Step 4: 运行测试确认通过**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~FileManager" -c Debug`
Expected: PASS(新增 2 个 + 原 11 个全绿)。

- [ ] **Step 5: 提交**

```bash
git add src/VivoKsu.App/ViewModels/FileManagerViewModel.cs tests/VivoKsu.App.Tests/FileManagerViewModelTests.cs
git commit -m "feat: 文件管理传出文件弹保存位置对话框(可注入选择器)"
```

---

### Task 3: `MainViewModel` 账号、时钟、登出命令

**Files:**
- Modify: `src/VivoKsu.App/ViewModels/MainViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/MainViewModelTests.cs`

**Interfaces:**
- Consumes: 现有 ctor;`Application.Current`(System.Windows)。
- Produces:
  - `[ObservableProperty] string AccountName`(Task 4 设置)
  - `[ObservableProperty] string CurrentTimeText`
  - `IAsyncRelayCommand LogoutCommand`
  - `void StopClock()`(Task 4 的 `StopAsync` 调用)
  - ctor 新增可选参数 `Func<Task>? onLogout = null`(Task 4 注入 `AppComposition.OnLogoutAsync`)

- [ ] **Step 1: 写失败测试**

```csharp
[Fact]
public async Task LogoutCommand_invokes_the_injected_logout_callback()
{
    var invoked = false;
    var viewModel = new MainViewModel(new DeviceSessionViewModel(), onLogout: () => { invoked = true; return Task.CompletedTask; });

    await viewModel.LogoutCommand.ExecuteAsync(null);

    Assert.True(invoked);
}

[Fact]
public void AccountName_is_settable()
{
    var viewModel = new MainViewModel(new DeviceSessionViewModel());

    viewModel.AccountName = "alice";

    Assert.Equal("alice", viewModel.AccountName);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~LogoutCommand|FullyQualifiedName~AccountName" -c Debug`
Expected: FAIL(编译失败:`LogoutCommand`/`AccountName`/`onLogout` 不存在)。

- [ ] **Step 3: 实现**

文件头加 `using System.Windows;`。字段与属性:

```csharp
private readonly Func<Task>? onLogout;
private readonly System.Windows.Threading.DispatcherTimer? clockTimer;

[ObservableProperty]
private string accountName = "";

[ObservableProperty]
private string currentTimeText = "";

public IAsyncRelayCommand LogoutCommand { get; }
```

ctor 末尾追加参数与初始化(在现有最后一个参数 `SoftwareViewModel? software = null` 之后):

```csharp
    Func<Task>? onLogout = null)
{
    // ...现有赋值...
    this.onLogout = onLogout;
    LogoutCommand = new AsyncRelayCommand(LogoutAsync);
    // 纯单测环境(无 WPF Application)不启动时钟,避免 DispatcherTimer 泄漏。
    if (Application.Current is not null)
    {
        clockTimer = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        clockTimer.Tick += (_, _) => CurrentTimeText = DateTime.Now.ToString("MM-dd HH:mm:ss");
        clockTimer.Start();
        CurrentTimeText = DateTime.Now.ToString("MM-dd HH:mm:ss");
    }
    SelectPageCommand = new RelayCommand<AppPage>(page => SelectedPage = page);
    RefreshDeviceCommand = new AsyncRelayCommand(() => RefreshDeviceAsync(logActivity: true));
}
```

> 注意:把 `SelectPageCommand`/`RefreshDeviceCommand` 的初始化保持原位置;新增代码放在它们之前或之后均可,但必须放在 ctor 末尾块内、`this.onLogout = onLogout;` 之后。

新增私有方法与公开停止时钟:

```csharp
private async Task LogoutAsync()
{
    if (onLogout is not null)
    {
        await onLogout();
    }
}

public void StopClock() => clockTimer?.Stop();
```

- [ ] **Step 4: 运行测试确认通过**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~MainViewModel" -c Debug`
Expected: PASS(新增 2 个 + 原 4 个全绿)。

- [ ] **Step 5: 提交**

```bash
git add src/VivoKsu.App/ViewModels/MainViewModel.cs tests/VivoKsu.App.Tests/MainViewModelTests.cs
git commit -m "feat: 主视图模型新增账号/时钟/登出命令"
```

---

### Task 4: `AppComposition` 登出接线 + 登录后设账号

**Files:**
- Modify: `src/VivoKsu.App/Services/AppComposition.cs`
- Test: `tests/VivoKsu.App.Tests/AppCompositionTests.cs`

**Interfaces:**
- Consumes: Task 3 的 `MainViewModel.LogoutCommand`/`AccountName`/`StopClock`。
- Produces:
  - `public event EventHandler? LogoutRequested`(Task 5 订阅)
  - `StartSessionAsync` 内设置 `MainViewModel.AccountName`
  - 私有 `OnLogoutAsync()`(注入给 MainViewModel)

- [ ] **Step 1: 写失败测试**(追加到 `AppCompositionTests.cs`,不启动会话故无网络调用;heartbeat `Start` 首次 tick 在 5s 后,且本测试根本不 `StartSessionAsync`)

```csharp
[Fact]
public async Task Logout_command_stops_the_composition_and_raises_logout_requested()
{
    var composition = AppComposition.CreateForTesting(new EmptyNativeApi(), new FakeProcessRunner());
    var logoutRaised = false;
    composition.LogoutRequested += (_, _) => logoutRaised = true;

    await composition.MainViewModel.LogoutCommand.ExecuteAsync(null);

    Assert.True(logoutRaised);
    Assert.False(composition.Heartbeat.IsRunning);
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~Logout_command_stops" -c Debug`
Expected: FAIL(编译失败:`LogoutRequested` 不存在;或运行失败:命令未接线)。

- [ ] **Step 3: 实现**

新增事件:

```csharp
/// <summary>用户点击登出,优雅下线完成后触发;App 据此关主窗回登录窗。</summary>
public event EventHandler? LogoutRequested;
```

`StartSessionAsync` 里设置账号(在 `CurrentUsername = username;` 后):

```csharp
CurrentUsername = username;
MainViewModel.AccountName = username;
```

`StopAsync` 顶部(在 `stopped = true;` 后)停时钟:

```csharp
stopped = true;
MainViewModel.StopClock();
```

MainViewModel 构造处加 `onLogout`(现有调用 `new MainViewModel(...)` 的命名参数里追加):

```csharp
MainViewModel = new MainViewModel(
    Session,
    overview,
    new OperationLogViewModel(LogService),
    deviceSessionService,
    quickFlash,
    mirror,
    fileManager,
    lineFlash,
    root,
    partitionWorkspace,
    firmwareExtract,
    safeFlash,
    Monitor,
    Coordinator,
    Online,
    new SoftwareViewModel(
        AppContext.BaseDirectory,
        preferences: toolPreferences,
        onReinstallDriver: () => new DriverReminderWindow(reinstallMode: true).ShowDialog()),
    onLogout: OnLogoutAsync);
```

新增私有方法(放在 `StopAsync` 附近):

```csharp
private async Task OnLogoutAsync()
{
    // 优雅下线(心跳 goodbye / 使用日志 flush / 停设备监视),完成后通知 App 回登录窗。
    await StopAsync();
    LogoutRequested?.Invoke(this, EventArgs.Empty);
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~AppComposition" -c Debug`
Expected: PASS(新增 1 个 + 原 3 个全绿)。

- [ ] **Step 5: 提交**

```bash
git add src/VivoKsu.App/Services/AppComposition.cs tests/VivoKsu.App.Tests/AppCompositionTests.cs
git commit -m "feat: AppComposition 登出优雅下线接线 + 登录后设账号"
```

---

### Task 5: `App.xaml.cs` 同进程登出回登录窗

**Files:**
- Modify: `src/VivoKsu.App/App.xaml.cs`

**Interfaces:**
- Consumes: Task 4 的 `AppComposition.LogoutRequested`。
- Produces: `OnStartup` 重构(登录循环可重入),`ShutdownMode = OnExplicitShutdown`。

无单测,以 `dotnet build` 通过为准。

- [ ] **Step 1: 重写 `OnStartup` + 新增登录循环与登出处理**

把现有 `OnStartup`(App.xaml.cs:35-96)的"登录门禁 → composition → MainWindow → Closed→Shutdown"部分重构为:

```csharp
protected override void OnStartup(StartupEventArgs eventArgs)
{
    base.OnStartup(eventArgs);

    // 崩溃日志(商业工具排查):记录未捕获异常到本地文件。
    DispatcherUnhandledException += (_, e) =>
    {
        if (e.Exception is UpdateRequiredException update)
        {
            WriteCrashLog(e.Exception);
            e.Handled = true;
            ShowUpdateRequired(update.Latest, update.MinVersion, update.DownloadUrl);
            Shutdown();
            return;
        }

        WriteCrashLog(e.Exception);
        e.Handled = true;
        MessageBox.Show("发生错误: " + e.Exception.Message, "奶娃Flash", MessageBoxButton.OK, MessageBoxImage.Error);
    };
    AppDomain.CurrentDomain.UnhandledException += (_, e) =>
        WriteCrashLog(e.ExceptionObject as Exception);

    // 版本门禁:打开软件即校验;版本低于后台「版本号控制」最低版本 → 强制更新,不进登录。
    if (BlockForForcedUpdate())
    {
        Shutdown();
        return;
    }

    // 登出后要回到登录窗而不退出程序:关窗不再自动退出,由代码显式 Shutdown。
    ShutdownMode = ShutdownMode.OnExplicitShutdown;
    RunApplicationLoop();
}

private bool isLogout;

/// <summary>登录循环:登录成功 → 新 composition + 主窗;登出 → 关闭主窗后重入本循环;退出 → Shutdown。</summary>
private void RunApplicationLoop()
{
    try
    {
        using var loginService = new LoginService();
        var login = new LoginWindow(loginService);
        if (login.ShowDialog() != true)
        {
            Shutdown();
            return;
        }

        var token = login.Token;

        composition = AppComposition.CreateDefault();
        composition.LogoutRequested += OnLogoutRequested;
        // 注入 token + 启动在线会话(心跳 / 强制下线监听 / 在线状态轮询)。
        composition.StartSessionAsync(token!, login.Username ?? string.Empty);
        var mainWindow = new MainWindow(composition);
        mainWindow.Closed += OnMainWindowClosed;
        MainWindow = mainWindow;
        mainWindow.Show();

        // 驱动提醒:后台检测手机 USB 驱动,未安装则弹「安装/取消」窗(不阻塞主界面)。
        CheckAndRemindDriverAsync();
    }
    catch (UpdateRequiredException update)
    {
        // 登录请求返回 426(绕过启动校验的兜底路径):强制更新。
        ShowUpdateRequired(update.Latest, update.MinVersion, update.DownloadUrl);
        Shutdown();
    }
}

private void OnLogoutRequested(object? sender, EventArgs eventArgs)
{
    isLogout = true;
    MainWindow?.Close();
}

private void OnMainWindowClosed(object? sender, EventArgs eventArgs)
{
    if (isLogout)
    {
        isLogout = false;
        RunApplicationLoop();
    }
    else
    {
        Shutdown();
    }
}
```

> 保留原有 `BlockForForcedUpdate` / `ShowUpdateRequired` / `CheckAndRemindDriverAsync` / `OnExit` 方法不动(OnExit 里 `composition.StopAsync()` 已被 Task 4 的 `stopped` 幂等守卫覆盖,登出后重复调用直接返回)。

- [ ] **Step 2: 构建**

Run: `dotnet build src/VivoKsu.App/VivoKsu.App.csproj -c Debug`
Expected: 编译成功。

- [ ] **Step 3: 提交**

```bash
git add src/VivoKsu.App/App.xaml.cs
git commit -m "feat: 同进程登出——登出后优雅下线并回到登录窗口"
```

---

### Task 6: 左侧导航重排 + 底部账号栏(MainWindow.xaml)

**Files:**
- Modify: `src/VivoKsu.App/MainWindow.xaml`(左侧导航 Border,当前行 56-85)

无单测,以 `dotnet build` 通过为准。

- [ ] **Step 1: 左侧导航重排 + 底部账号栏**

把现有左导航 `Border`(Grid.Row=1,Grid.Column=0,行 56-85)整体替换。要点:
- 内部 Grid 改两行:`*`(导航)+ `Auto`(底部账号栏)。
- 导航按钮按**刷机链路分组**重排:`设备概览 / 文件管理 / ADB 投屏` → 分隔线 → `快速刷写 / 可视刷写 / VIVO 线刷 / 固件提取 / Vivo ROOT` → 分隔线 → `在线状态 / 软件`。
- 每个按钮保留各自的 `SelectPageCommand` + `CommandParameter` 与选中态 DataTrigger(只改出现顺序,不改绑定)。

替换后的结构(按钮内联样式与原文一致,此处仅给出骨架与新增底部栏):

```xml
<Border Grid.Row="1" Background="{StaticResource RailBrush}" BorderBrush="{StaticResource EdgeBrush}" BorderThickness="0,0,1,0">
  <Grid>
    <Grid.RowDefinitions>
      <RowDefinition Height="*"/>
      <RowDefinition Height="Auto"/>
    </Grid.RowDefinitions>
    <StackPanel Margin="12,25,12,16">
      <TextBlock Text="WORKSPACE" FontFamily="Cascadia Mono" FontWeight="SemiBold" FontSize="9" Foreground="{StaticResource MutedBrush}" Margin="13,0,0,12"/>

      <!-- 设备概览(AppPage.Overview) -->
      <Button Content="设备概览" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.Overview}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.Overview}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <!-- 文件管理(AppPage.FileTransfer) -->
      <Button Content="文件管理" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.FileTransfer}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.FileTransfer}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <!-- ADB 投屏(AppPage.AdbActions) -->
      <Button Content="ADB 投屏" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.AdbActions}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.AdbActions}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <Border Height="1" Background="{StaticResource EdgeBrush}" Margin="13,16,13,14"/>

      <!-- 快速刷写(AppPage.FastbootFlash) -->
      <Button Content="快速刷写" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.FastbootFlash}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.FastbootFlash}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <!-- 可视刷写(AppPage.LineFlash) -->
      <Button Content="可视刷写" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.LineFlash}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.LineFlash}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <!-- VIVO 线刷(AppPage.SafeFlash) -->
      <Button Content="VIVO 线刷" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.SafeFlash}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.SafeFlash}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <!-- 固件提取(AppPage.FirmwareExtract) -->
      <Button Content="固件提取" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.FirmwareExtract}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.FirmwareExtract}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <!-- Vivo ROOT(AppPage.RootTools) -->
      <Button Content="Vivo ROOT" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.RootTools}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.RootTools}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <Border Height="1" Background="{StaticResource EdgeBrush}" Margin="13,16,13,14"/>

      <!-- 在线状态(AppPage.OnlineStatus) -->
      <Button Content="在线状态" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.OnlineStatus}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.OnlineStatus}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>

      <!-- 软件(AppPage.Software) -->
      <Button Content="软件" Command="{Binding SelectPageCommand}" CommandParameter="{x:Static models:AppPage.Software}">
        <Button.Style><Style TargetType="Button" BasedOn="{StaticResource NavButtonStyle}"><Style.Triggers><DataTrigger Binding="{Binding SelectedPage}" Value="{x:Static models:AppPage.Software}"><Setter Property="Background" Value="#EAF7F5"/><Setter Property="BorderBrush" Value="{StaticResource ConnectionBrush}"/><Setter Property="BorderThickness" Value="3,0,0,0"/><Setter Property="Foreground" Value="#08766E"/><Setter Property="FontWeight" Value="SemiBold"/></DataTrigger></Style.Triggers></Style></Button.Style>
      </Button>
    </StackPanel>

    <!-- 底部账号栏:账号 id + 当前时间 + 登出 -->
    <Border Grid.Row="1" Background="#F2F8F7" BorderBrush="{StaticResource EdgeBrush}" BorderThickness="0,1,0,0" Padding="12,10">
      <StackPanel>
        <Grid>
          <Grid.ColumnDefinitions><ColumnDefinition Width="Auto"/><ColumnDefinition Width="*"/></Grid.ColumnDefinitions>
          <TextBlock Text="账号 " FontSize="9" Foreground="{StaticResource MutedBrush}" VerticalAlignment="Center"/>
          <TextBlock Grid.Column="1" Text="{Binding AccountName}" FontSize="10" FontWeight="SemiBold" TextTrimming="CharacterEllipsis" VerticalAlignment="Center"/>
        </Grid>
        <TextBlock Text="{Binding CurrentTimeText}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}" Margin="0,4,0,0"/>
        <Button Content="登出" Style="{StaticResource ToolButtonStyle}" Command="{Binding LogoutCommand}" Margin="0,8,0,0" Padding="0,5" HorizontalContentAlignment="Center"/>
      </StackPanel>
    </Border>
  </Grid>
</Border>
```

- [ ] **Step 2: 构建**

Run: `dotnet build src/VivoKsu.App/VivoKsu.App.csproj -c Debug`
Expected: 编译成功(绑定到新 `AccountName`/`CurrentTimeText`/`LogoutCommand`)。

- [ ] **Step 3: 提交**

```bash
git add src/VivoKsu.App/MainWindow.xaml
git commit -m "feat: 左侧菜单按刷机链路重排 + 左下角账号/时间/登出栏"
```

---

### Task 7: 右上角统一操作进度区 + 页面移除进度条(MainWindow.xaml)

**Files:**
- Modify: `src/VivoKsu.App/MainWindow.xaml`
  - 右上 DEVICE STATUS 卡片进度区(当前行 1446-1528)
  - 固件提取页底部面板(当前行 982-1007)
  - 文件管理页行 4 底栏(当前行 685)

无单测,以 `dotnet build` 通过为准。

- [ ] **Step 1: 重写右上进度区**

把当前 DEVICE STATUS 卡片内三个按 `SelectedPage` 显示的 StackPanel(行 1446-1528,含其后的分隔线)整体替换为:**固定的「操作进度」标题 + 占位块 + 五块按忙显示的操作块**。

替换后结构:

```xml
<Border Height="1" Background="{StaticResource EdgeBrush}" Margin="0,12,0,10"/>
<TextBlock Text="操作进度" FontFamily="Cascadia Mono" FontSize="9" FontWeight="SemiBold" Foreground="{StaticResource ConnectionBrush}"/>

<!-- 占位:全部空闲时显示 -->
<StackPanel Margin="0,10,0,0">
  <StackPanel.Style>
    <Style TargetType="StackPanel">
      <Setter Property="Visibility" Value="Collapsed"/>
      <Style.Triggers>
        <MultiDataTrigger>
          <MultiDataTrigger.Conditions>
            <Condition Binding="{Binding QuickFlash.IsFlashOperationActive}" Value="False"/>
            <Condition Binding="{Binding PartitionWorkspace.IsExecuting}" Value="False"/>
            <Condition Binding="{Binding SafeFlash.IsBusy}" Value="False"/>
            <Condition Binding="{Binding FirmwareExtract.IsPayloadBusy}" Value="False"/>
            <Condition Binding="{Binding DeviceSession.IsBusy}" Value="False"/>
          </MultiDataTrigger.Conditions>
          <Setter Property="Visibility" Value="Visible"/>
        </MultiDataTrigger>
      </Style.Triggers>
    </Style>
  </StackPanel.Style>
  <TextBlock Text="无进行中的操作" FontSize="11" Foreground="{StaticResource MutedBrush}"/>
</StackPanel>

<!-- 快速刷写(运行中显示) -->
<StackPanel Margin="0,10,0,0">
  <StackPanel.Style>
    <Style TargetType="StackPanel">
      <Setter Property="Visibility" Value="Collapsed"/>
      <Style.Triggers>
        <DataTrigger Binding="{Binding QuickFlash.IsFlashOperationActive}" Value="True"><Setter Property="Visibility" Value="Visible"/></DataTrigger>
      </Style.Triggers>
    </Style>
  </StackPanel.Style>
  <Grid>
    <TextBlock Text="当前分区" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding QuickFlash.CurrentPartition}" FontFamily="Cascadia Mono" FontSize="9" FontWeight="SemiBold" Foreground="{StaticResource TextBrush}" Margin="56,0,0,0" TextTrimming="CharacterEllipsis"/>
    <StackPanel Orientation="Horizontal" HorizontalAlignment="Right">
      <TextBlock Text="{Binding QuickFlash.CurrentPartitionProgressPercent}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource ConnectionBrush}"/>
      <TextBlock Text=" · " FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
      <TextBlock Text="{Binding QuickFlash.SpeedText}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource TextBrush}"/>
    </StackPanel>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding QuickFlash.CurrentPartitionProgress}" IsIndeterminate="{Binding QuickFlash.IsCurrentPartitionIndeterminate}" Margin="0,6,0,0"/>
  <Grid Margin="0,10,0,0">
    <TextBlock Text="总进度" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding QuickFlash.OverallProgressPercent}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource ConnectionBrush}" HorizontalAlignment="Right"/>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding QuickFlash.OverallProgress}" Margin="0,6,0,0"/>
</StackPanel>

<!-- 可视刷写(运行中显示) -->
<StackPanel Margin="0,10,0,0">
  <StackPanel.Style>
    <Style TargetType="StackPanel">
      <Setter Property="Visibility" Value="Collapsed"/>
      <Style.Triggers>
        <DataTrigger Binding="{Binding PartitionWorkspace.IsExecuting}" Value="True"><Setter Property="Visibility" Value="Visible"/></DataTrigger>
      </Style.Triggers>
    </Style>
  </StackPanel.Style>
  <Grid>
    <TextBlock Text="当前分区" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding PartitionWorkspace.CurrentOperationPartitionName}" FontFamily="Cascadia Mono" FontSize="9" FontWeight="SemiBold" Foreground="{StaticResource TextBrush}" Margin="56,0,0,0" TextTrimming="CharacterEllipsis"/>
    <StackPanel Orientation="Horizontal" HorizontalAlignment="Right">
      <TextBlock Text="{Binding PartitionWorkspace.OperationProgressPercent}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource ConnectionBrush}"/>
      <TextBlock Text=" · " FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
      <TextBlock Text="{Binding PartitionWorkspace.OperationSpeedText}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource TextBrush}"/>
    </StackPanel>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding PartitionWorkspace.CurrentOperationProgress}" IsIndeterminate="{Binding PartitionWorkspace.IsCurrentOperationIndeterminate}" Margin="0,6,0,0"/>
  <Grid Margin="0,10,0,0">
    <TextBlock Text="总进度" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding PartitionWorkspace.OperationProgressPercent}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource ConnectionBrush}" HorizontalAlignment="Right"/>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding PartitionWorkspace.OverallProgress}" Margin="0,6,0,0"/>
</StackPanel>

<!-- VIVO 线刷(运行中显示) -->
<StackPanel Margin="0,10,0,0">
  <StackPanel.Style>
    <Style TargetType="StackPanel">
      <Setter Property="Visibility" Value="Collapsed"/>
      <Style.Triggers>
        <DataTrigger Binding="{Binding SafeFlash.IsBusy}" Value="True"><Setter Property="Visibility" Value="Visible"/></DataTrigger>
      </Style.Triggers>
    </Style>
  </StackPanel.Style>
  <Grid>
    <TextBlock Text="当前分区" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding SafeFlash.CurrentPartition}" FontFamily="Cascadia Mono" FontSize="9" FontWeight="SemiBold" Foreground="{StaticResource TextBrush}" Margin="56,0,0,0" TextTrimming="CharacterEllipsis"/>
    <StackPanel Orientation="Horizontal" HorizontalAlignment="Right">
      <TextBlock Text="{Binding SafeFlash.CurrentPartitionProgressPercent}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource ConnectionBrush}"/>
      <TextBlock Text=" · " FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
      <TextBlock Text="{Binding SafeFlash.SpeedText}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource TextBrush}"/>
    </StackPanel>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding SafeFlash.CurrentPartitionProgress}" IsIndeterminate="{Binding SafeFlash.IsCurrentPartitionIndeterminate}" Margin="0,6,0,0"/>
  <Grid Margin="0,10,0,0">
    <TextBlock Text="总进度" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding SafeFlash.OverallProgressPercent}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource ConnectionBrush}" HorizontalAlignment="Right"/>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding SafeFlash.OverallProgress}" Margin="0,6,0,0"/>
</StackPanel>

<!-- 固件提取(运行中显示) -->
<StackPanel Margin="0,10,0,0">
  <StackPanel.Style>
    <Style TargetType="StackPanel">
      <Setter Property="Visibility" Value="Collapsed"/>
      <Style.Triggers>
        <DataTrigger Binding="{Binding FirmwareExtract.IsPayloadBusy}" Value="True"><Setter Property="Visibility" Value="Visible"/></DataTrigger>
      </Style.Triggers>
    </Style>
  </StackPanel.Style>
  <Grid>
    <TextBlock Text="当前分区" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding FirmwareExtract.CurrentPartitionName}" FontFamily="Cascadia Mono" FontSize="9" FontWeight="SemiBold" Foreground="{StaticResource TextBrush}" Margin="56,0,0,0" TextTrimming="CharacterEllipsis"/>
    <StackPanel Orientation="Horizontal" HorizontalAlignment="Right">
      <TextBlock Text="{Binding FirmwareExtract.SpeedText}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource TextBrush}"/>
      <TextBlock Text=" · " FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
      <TextBlock Text="{Binding FirmwareExtract.ElapsedText}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource TextBrush}"/>
    </StackPanel>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding FirmwareExtract.CurrentPartitionProgress}" IsIndeterminate="{Binding FirmwareExtract.IsCurrentPartitionIndeterminate}" Margin="0,6,0,0"/>
  <Grid Margin="0,10,0,0">
    <TextBlock Text="总进度" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding FirmwareExtract.PayloadProgressPercent}" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource ConnectionBrush}" HorizontalAlignment="Right"/>
  </Grid>
  <ProgressBar Height="5" Minimum="0" Maximum="1" Value="{Binding FirmwareExtract.PayloadProgress}" Margin="0,6,0,0"/>
</StackPanel>

<!-- 设备操作(通用:文件传输/ROOT 等仅上报阶段的操作;四块特定页均空闲时显示) -->
<StackPanel Margin="0,10,0,0">
  <StackPanel.Style>
    <Style TargetType="StackPanel">
      <Setter Property="Visibility" Value="Collapsed"/>
      <Style.Triggers>
        <MultiDataTrigger>
          <MultiDataTrigger.Conditions>
            <Condition Binding="{Binding DeviceSession.IsBusy}" Value="True"/>
            <Condition Binding="{Binding QuickFlash.IsFlashOperationActive}" Value="False"/>
            <Condition Binding="{Binding PartitionWorkspace.IsExecuting}" Value="False"/>
            <Condition Binding="{Binding SafeFlash.IsBusy}" Value="False"/>
            <Condition Binding="{Binding FirmwareExtract.IsPayloadBusy}" Value="False"/>
          </MultiDataTrigger.Conditions>
          <Setter Property="Visibility" Value="Visible"/>
        </MultiDataTrigger>
      </Style.Triggers>
    </Style>
  </StackPanel.Style>
  <Grid>
    <TextBlock Text="当前操作" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}"/>
    <TextBlock Text="{Binding DeviceSession.StatusText}" FontFamily="Cascadia Mono" FontSize="9" FontWeight="SemiBold" Foreground="{StaticResource TextBrush}" Margin="56,0,0,0" TextTrimming="CharacterEllipsis"/>
  </Grid>
  <ProgressBar Height="5" IsIndeterminate="True" Margin="0,6,0,0"/>
</StackPanel>
```

> 必须删掉原行 1446 附近残留的「操作进度」注释里的三个旧 StackPanel,避免重复绑定与重复显示。

- [ ] **Step 2: 移除固件提取页底部双进度条**

当前固件提取底部面板(行 982-1007)的 Grid 列定义为 `ColumnDefinition/>/280/Auto`。改为两列,并删除 Column 1 的 StackPanel(「当前分区」标签 + `CurrentPartitionProgress` 进度条 + 「总进度」标签 + `PayloadProgressPercent` + `PayloadProgress` 进度条),保留 Column 0 状态文案与 Column 2 按钮。

替换后(保留其余部分):

```xml
<Border Grid.Row="2" Style="{StaticResource PanelBorderStyle}" Padding="16,12" Margin="0,14,0,0">
  <Grid>
    <Grid.ColumnDefinitions><ColumnDefinition/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
    <StackPanel VerticalAlignment="Center">
      <TextBlock Text="{Binding FirmwareExtract.PayloadStatusText}" FontSize="12" FontWeight="SemiBold" TextTrimming="CharacterEllipsis"/>
      <TextBlock FontSize="10" Foreground="{StaticResource MutedBrush}" Margin="0,4,0,0">
        <Run Text="速度  "/><Run Text="{Binding FirmwareExtract.SpeedText, Mode=OneWay}"/><Run Text="      耗时  "/><Run Text="{Binding FirmwareExtract.ElapsedText, Mode=OneWay}"/>
      </TextBlock>
    </StackPanel>
    <StackPanel Grid.Column="1" Orientation="Horizontal" VerticalAlignment="Center">
      <Button Content="读取信息" Style="{StaticResource ToolButtonStyle}" Command="{Binding FirmwareExtract.ReadInfoCommand}" AutomationProperties.Name="读取信息"/>
      <Button Content="提取镜像" Style="{StaticResource PrimaryButtonStyle}" Command="{Binding FirmwareExtract.ExtractCommand}" AutomationProperties.Name="提取镜像"/>
      <Button Content="映射到快速刷写" Style="{StaticResource SoftTealButtonStyle}" Command="{Binding FirmwareExtract.MapToQuickFlashCommand}" AutomationProperties.Name="映射到快速刷写"/>
      <Button Content="停止操作" Style="{StaticResource SignalButtonStyle}" Command="{Binding FirmwareExtract.StopCommand}" AutomationProperties.Name="停止操作" Margin="0"/>
    </StackPanel>
  </Grid>
</Border>
```

- [ ] **Step 3: 移除文件管理页行 4 忙进度条**

当前文件管理页行 4 底栏(行 685)Grid 有 3 列(状态文案 / 忙进度条 / 「ADB 文件传输」标签)。删除中间的 `ProgressBar`(绑定 `DeviceSession.IsBusy`),保留状态文案与标签,Grid 改两列:

```xml
<Border Grid.Row="4" Background="#F7F9FB" BorderBrush="{StaticResource EdgeBrush}" BorderThickness="0,1,0,0" Padding="12,0">
  <Grid>
    <Grid.ColumnDefinitions><ColumnDefinition/><ColumnDefinition Width="Auto"/></Grid.ColumnDefinitions>
    <TextBlock Text="{Binding DeviceSession.StatusText}" Foreground="{Binding DeviceSession.ConnectionAccentBrush}" FontSize="10" VerticalAlignment="Center" TextTrimming="CharacterEllipsis"/>
    <TextBlock Grid.Column="1" Text="ADB 文件传输" FontFamily="Cascadia Mono" FontSize="9" Foreground="{StaticResource MutedBrush}" VerticalAlignment="Center"/>
  </Grid>
</Border>
```

- [ ] **Step 4: 构建**

Run: `dotnet build src/VivoKsu.App/VivoKsu.App.csproj -c Debug`
Expected: 编译成功。

- [ ] **Step 5: 提交**

```bash
git add src/VivoKsu.App/MainWindow.xaml
git commit -m "refactor: 全部主进度统一到右上角操作进度区(固件提取/文件传输入位,页面移除重复条)"
```

---

### Task 8: 全量测试 + 文档 + 最终提交

**Files:**
- Modify: `README.md`、`docs/index.md`(页面描述同步)

- [ ] **Step 1: 跑全量测试**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj -c Debug`
Expected: 全部 PASS(原 339 + 新增 6 = **345**)。若出现失败,先修复再提交,不得跳过。

- [ ] **Step 2: 更新文档**

在 `README.md` 功能页面表中,更新「文件管理」行为说明为「传出文件弹保存位置对话框」;「在线状态」附近(如有登出描述)不涉及。「VIVO 线刷」等不变。同步更新 `docs/index.md` 中桌面端能力描述(如有提及固定下载目录,改为保存对话框)。

- [ ] **Step 3: 复核命名约定与残留**

- 新增 UI 文案只出现「奶娃Flash」,无 "Nwflash"。
- `git status` 检查无遗漏/无多余文件(尤其不要把 `bin/`、`obj/` 提交)。

- [ ] **Step 4: 提交**

```bash
git add README.md docs/index.md
git commit -m "docs: 同步界面整合改动(保存对话框/统一进度区/登出/菜单重排)"
```

---

## Self-Review 结论(写计划时已核对)

- **Spec 覆盖**:①=Task2,②④=Task7,③=Task3/4/5/6,⑤=Task6;决策记录(按刷机链路分组/同进程登出/只搬主进度条)全部落进对应 Task。✓
- **占位符**:所有代码步骤均含真实代码,无 TBD/TODO。✓
- **类型一致**:`DownloadToFileAsync`、`saveLocationPicker`、`onLogout`、`AccountName`、`CurrentTimeText`、`LogoutCommand`、`StopClock`、`LogoutRequested` 在 Task 1-6 间签名一致。✓
- **测试隔离**:AppComposition 登出测试不启动会话(heartbeat `Start` 首 tick 延迟 5s,未 start 则 goodbye 因 sessionId 为 null 直接返回)→ 无网络调用。✓
