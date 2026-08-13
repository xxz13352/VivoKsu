# VivoKsu Device Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a .NET 8 WPF device utility with bundled ADB/Fastboot, fixed left-bottom device status, file transfer, device inspection, and safe single-partition flashing.

**Architecture:** A WPF MVVM shell hosts five feature pages and a fixed device-status region in the left rail. `CommandRunner` owns child-process execution; typed ADB/Fastboot services construct commands and parse results. A shared `DeviceSessionViewModel` drives the fixed status panel and command availability.

**Tech Stack:** .NET 8, WPF, CommunityToolkit.Mvvm, Wpf.Ui, xUnit, FluentAssertions.

## Global Constraints

- Target Windows with `net8.0-windows` and WPF enabled.
- Bundle `adb.exe`, `fastboot.exe`, and their required Platform Tools DLLs under `platform-tools/` beside the executable.
- Support exactly one connected device; multiple devices disable execution actions.
- Use one fixed left-bottom device panel; do not replace the main workspace lower area with global operation status.
- Keep command invocation asynchronous, cancellable, and captured in the operation log.
- Use Graphite and Teal Fluent styling with accessible keyboard focus, empty, loading, error, and disabled states.

---

### Task 1: Create the solution shell and visual foundation

**Files:**
- Create: `VivoKsu.sln`
- Create: `src/VivoKsu.App/VivoKsu.App.csproj`
- Create: `src/VivoKsu.App/App.xaml`
- Create: `src/VivoKsu.App/MainWindow.xaml`
- Create: `src/VivoKsu.App/MainWindow.xaml.cs`
- Create: `src/VivoKsu.App/ViewModels/MainViewModel.cs`
- Create: `tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj`
- Create: `tests/VivoKsu.App.Tests/MainViewModelTests.cs`

**Interfaces:**
- Produces `MainViewModel` with `SelectedPage`, `DeviceSession`, and navigation commands.

- [ ] **Step 1: Write the failing navigation test**

```csharp
[Fact]
public void Selecting_a_page_updates_the_current_page()
{
    var viewModel = new MainViewModel(new DeviceSessionViewModel());

    viewModel.SelectPageCommand.Execute(AppPage.FileTransfer);

    viewModel.SelectedPage.Should().Be(AppPage.FileTransfer);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter Selecting_a_page_updates_the_current_page`

Expected: compilation failure because `MainViewModel`, `DeviceSessionViewModel`, and `AppPage` do not exist.

- [ ] **Step 3: Create the project, package references, and minimal view model**

```csharp
public enum AppPage { Overview, AdbActions, FileTransfer, FastbootFlash, OperationLog }

public partial class MainViewModel : ObservableObject
{
    [ObservableProperty] private AppPage selectedPage = AppPage.Overview;
    public DeviceSessionViewModel DeviceSession { get; }
    public IRelayCommand<AppPage> SelectPageCommand { get; }

    public MainViewModel(DeviceSessionViewModel deviceSession)
    {
        DeviceSession = deviceSession;
        SelectPageCommand = new RelayCommand<AppPage>(page => SelectedPage = page);
    }
}
```

Create `MainWindow` with a Wpf.Ui `NavigationView`, a content host bound to `SelectedPage`, and a fixed `DeviceStatusPanel` located at the bottom of the left navigation rail.

- [ ] **Step 4: Run the test to verify it passes**

Run: `dotnet test tests/VivoKsu.App.Tests --filter Selecting_a_page_updates_the_current_page`

Expected: one passing test.

### Task 2: Define shared device and operation state

**Files:**
- Create: `src/VivoKsu.App/Models/DeviceConnectionState.cs`
- Create: `src/VivoKsu.App/Models/OperationKind.cs`
- Create: `src/VivoKsu.App/Models/DeviceSnapshot.cs`
- Create: `src/VivoKsu.App/ViewModels/DeviceSessionViewModel.cs`
- Create: `tests/VivoKsu.App.Tests/DeviceSessionViewModelTests.cs`

**Interfaces:**
- Produces `DeviceSessionViewModel.BeginOperation(OperationKind, string)`, `CompleteOperation()`, and `FailOperation(string)`.
- Consumed by every page and the fixed left-bottom device panel.

- [ ] **Step 1: Write the failing operation-state test**

```csharp
[Fact]
public void Beginning_a_flash_updates_the_fixed_device_status()
{
    var session = new DeviceSessionViewModel();

    session.BeginOperation(OperationKind.Flashing, "正在刷写 boot.img");

    session.OperationKind.Should().Be(OperationKind.Flashing);
    session.StatusText.Should().Be("正在刷写 boot.img");
    session.IsBusy.Should().BeTrue();
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter Beginning_a_flash_updates_the_fixed_device_status`

Expected: compilation failure because the session state API is absent.

- [ ] **Step 3: Implement state transitions and XAML binding**

```csharp
public enum OperationKind { Idle, Discovering, Rebooting, Installing, Transferring, Hashing, Flashing, Completed, Canceled, Failed }

public partial class DeviceSessionViewModel : ObservableObject
{
    [ObservableProperty] private OperationKind operationKind = OperationKind.Idle;
    [ObservableProperty] private string statusText = "未检测到设备";
    [ObservableProperty] private bool isBusy;

    public void BeginOperation(OperationKind kind, string text)
    {
        OperationKind = kind;
        StatusText = text;
        IsBusy = true;
    }
}
```

Bind the status panel to model, mode, serial, battery, status text, and `IsBusy`; reserve the panel's layout height so text changes never move page content.

- [ ] **Step 4: Run the test to verify it passes**

Run: `dotnet test tests/VivoKsu.App.Tests --filter Beginning_a_flash_updates_the_fixed_device_status`

Expected: one passing test.

### Task 3: Build a tested process-execution boundary

**Files:**
- Create: `src/VivoKsu.App/Services/ICommandRunner.cs`
- Create: `src/VivoKsu.App/Services/CommandRunner.cs`
- Create: `src/VivoKsu.App/Models/CommandResult.cs`
- Create: `tests/VivoKsu.App.Tests/CommandRunnerTests.cs`

**Interfaces:**
- Produces `Task<CommandResult> RunAsync(string executable, IReadOnlyList<string> arguments, CancellationToken token)`.
- Consumed by Platform Tools, ADB, and Fastboot services.

- [ ] **Step 1: Write the failing command-output test**

```csharp
[Fact]
public async Task RunAsync_captures_stdout_stderr_and_exit_code()
{
    var runner = new CommandRunner();
    var result = await runner.RunAsync("cmd.exe", ["/c", "echo out & echo err 1>&2 & exit /b 7"], CancellationToken.None);

    result.ExitCode.Should().Be(7);
    result.StandardOutput.Should().Contain("out");
    result.StandardError.Should().Contain("err");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter RunAsync_captures_stdout_stderr_and_exit_code`

Expected: compilation failure because `CommandRunner` is absent.

- [ ] **Step 3: Implement async process execution**

Use `ProcessStartInfo.ArgumentList`, redirect standard output and error, read both asynchronously, await `WaitForExitAsync(token)`, and return the executable, arguments, exit code, output, error, and duration in `CommandResult`. On cancellation, kill the process tree before rethrowing `OperationCanceledException`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `dotnet test tests/VivoKsu.App.Tests --filter RunAsync_captures_stdout_stderr_and_exit_code`

Expected: one passing test.

### Task 4: Locate Platform Tools and discover one device

**Files:**
- Create: `src/VivoKsu.App/Services/PlatformToolsLocator.cs`
- Create: `src/VivoKsu.App/Services/AdbService.cs`
- Create: `src/VivoKsu.App/Services/FastbootService.cs`
- Create: `src/VivoKsu.App/Services/DeviceInfoService.cs`
- Create: `tests/VivoKsu.App.Tests/AdbServiceTests.cs`
- Create: `tests/VivoKsu.App.Tests/DeviceInfoServiceTests.cs`

**Interfaces:**
- Produces `Task<DeviceSnapshot> DiscoverAsync(CancellationToken token)` and `Task<DeviceSnapshot> ReadInfoAsync(string serial, CancellationToken token)`.
- Consumes `ICommandRunner` and paths provided by `PlatformToolsLocator`.

- [ ] **Step 1: Write the failing ADB parser test**

```csharp
[Fact]
public void ParseDevices_accepts_one_authorized_device()
{
    const string output = "List of devices attached\n1A2B3C4D device product:husky model:Pixel_8_Pro\n";

    var state = AdbService.ParseDevices(output);

    state.ConnectionState.Should().Be(DeviceConnectionState.AdbConnected);
    state.Serial.Should().Be("1A2B3C4D");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter ParseDevices_accepts_one_authorized_device`

Expected: compilation failure because the parser is absent.

- [ ] **Step 3: Implement platform validation and discovery**

Require `platform-tools/adb.exe` and `platform-tools/fastboot.exe` next to the executable. Parse `adb devices -l` states as disconnected, unauthorized, multiple devices, or ADB-connected. If no ADB device is found, parse `fastboot devices` before returning `FastbootConnected` or `Disconnected`. Read model, Android release, serial, battery, and storage through separate ADB commands.

- [ ] **Step 4: Run parser and fake-tool discovery tests**

Run: `dotnet test tests/VivoKsu.App.Tests --filter "ParseDevices|Discover"`

Expected: all discovery tests pass, including unauthorized and multi-device cases.

### Task 5: Implement the overview and ADB action page

**Files:**
- Create: `src/VivoKsu.App/Views/OverviewView.xaml`
- Create: `src/VivoKsu.App/ViewModels/OverviewViewModel.cs`
- Create: `src/VivoKsu.App/Views/AdbActionsView.xaml`
- Create: `src/VivoKsu.App/ViewModels/AdbActionsViewModel.cs`
- Create: `tests/VivoKsu.App.Tests/AdbActionsViewModelTests.cs`

**Interfaces:**
- Consumes `AdbService`, `DeviceSessionViewModel`, and `OperationLogViewModel`.
- Produces `RebootCommand`, `RebootRecoveryCommand`, `RebootBootloaderCommand`, and `InstallApkCommand`.

- [ ] **Step 1: Write the failing reboot-command test**

```csharp
[Fact]
public async Task RebootRecovery_sets_session_text_before_running_adb()
{
    var session = new DeviceSessionViewModel();
    var adb = new FakeAdbService();
    var viewModel = new AdbActionsViewModel(adb, session, new OperationLogViewModel());

    await viewModel.RebootRecoveryCommand.ExecuteAsync(null);

    adb.LastRebootTarget.Should().Be("recovery");
    session.StatusText.Should().Be("正在重启到 Recovery...");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter RebootRecovery_sets_session_text_before_running_adb`

Expected: compilation failure because the view model and command are absent.

- [ ] **Step 3: Implement ADB commands and page states**

Build the overview menu grid from the approved layout. The ADB page exposes normal reboot, Recovery, Bootloader, Fastbootd, APK selection, and `adb install -r`. Disable commands unless state is `AdbConnected`; append result output to the operation log and complete/fail the shared session state after every command.

- [ ] **Step 4: Run the ADB view model tests**

Run: `dotnet test tests/VivoKsu.App.Tests --filter "Reboot|Install"`

Expected: reboot and install state tests pass.

### Task 6: Implement visual file transfer

**Files:**
- Create: `src/VivoKsu.App/Models/RemoteFileEntry.cs`
- Create: `src/VivoKsu.App/Services/FileTransferService.cs`
- Create: `src/VivoKsu.App/Views/FileTransferView.xaml`
- Create: `src/VivoKsu.App/ViewModels/FileTransferViewModel.cs`
- Create: `tests/VivoKsu.App.Tests/FileTransferViewModelTests.cs`

**Interfaces:**
- Produces `RefreshRemoteDirectoryAsync`, `UploadAsync`, `DownloadAsync`, and `CancelTransferCommand`.
- Consumes `ICommandRunner`, `AdbService`, and `DeviceSessionViewModel`.

- [ ] **Step 1: Write the failing upload validation test**

```csharp
[Fact]
public async Task Upload_rejects_an_empty_remote_path()
{
    var viewModel = new FileTransferViewModel(new FakeFileTransferService(), new DeviceSessionViewModel(), new OperationLogViewModel());
    viewModel.SelectedLocalPath = "C:\\temp\\file.zip";
    viewModel.RemotePath = "";

    await viewModel.UploadCommand.ExecuteAsync(null);

    viewModel.ValidationMessage.Should().Be("请选择设备目标路径。");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter Upload_rejects_an_empty_remote_path`

Expected: compilation failure because the transfer view model is absent.

- [ ] **Step 3: Implement local/remote transfer behavior**

Render local selection and remote directory controls in a two-column page. Parse `adb shell ls -1 -p` into typed files/directories. Upload uses `adb push`; download uses `adb pull`. Bind process-output events to byte/speed text where reported, support cancellation tokens, ask before overwrite, and update only the fixed device panel plus local transfer details.

- [ ] **Step 4: Run transfer tests**

Run: `dotnet test tests/VivoKsu.App.Tests --filter "Upload|Download|RemoteDirectory"`

Expected: validation, command construction, and cancellation tests pass.

### Task 7: Implement Fastboot image validation and flashing

**Files:**
- Create: `src/VivoKsu.App/Models/FlashRequest.cs`
- Create: `src/VivoKsu.App/Services/HashService.cs`
- Create: `src/VivoKsu.App/Views/FastbootFlashView.xaml`
- Create: `src/VivoKsu.App/ViewModels/FastbootFlashViewModel.cs`
- Create: `tests/VivoKsu.App.Tests/FastbootFlashViewModelTests.cs`

**Interfaces:**
- Produces `CalculateHashCommand`, `FlashCommand`, and `IsFlashReady`.
- Consumes `FastbootService`, `HashService`, `DeviceSessionViewModel`, and the selected partition/image path.

- [ ] **Step 1: Write the failing partition guard test**

```csharp
[Fact]
public void Flash_is_disabled_for_a_partition_outside_the_allowlist()
{
    var viewModel = new FastbootFlashViewModel(new FakeFastbootService(), new HashService(), new DeviceSessionViewModel(), new OperationLogViewModel());
    viewModel.SelectedPartition = "radio";
    viewModel.ImagePath = "C:\\images\\radio.img";

    viewModel.IsFlashReady.Should().BeFalse();
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter Flash_is_disabled_for_a_partition_outside_the_allowlist`

Expected: compilation failure because the Fastboot flash view model is absent.

- [ ] **Step 3: Implement the flash workflow**

Expose the approved partition allowlist, accept only `.img` files, compute SHA-256 with `SHA256.HashDataAsync`, require `FastbootConnected`, show image, partition, hash, and serial in a confirmation region, then run `fastboot flash <partition> <image>`. Stream output into the log and preserve the selected image and hash after completion or failure.

- [ ] **Step 4: Run Fastboot view model tests**

Run: `dotnet test tests/VivoKsu.App.Tests --filter "Flash|Hash|Partition"`

Expected: allowlist, file type, device mode, hash, and command tests pass.

### Task 8: Add operation log, packaging, and verification

**Files:**
- Create: `src/VivoKsu.App/Models/OperationLogEntry.cs`
- Create: `src/VivoKsu.App/ViewModels/OperationLogViewModel.cs`
- Create: `src/VivoKsu.App/Views/OperationLogView.xaml`
- Create: `src/VivoKsu.App/platform-tools/.gitkeep`
- Create: `README.md`
- Modify: `src/VivoKsu.App/VivoKsu.App.csproj`
- Create: `tests/VivoKsu.App.Tests/OperationLogViewModelTests.cs`

**Interfaces:**
- Produces `OperationLogViewModel.Append(CommandResult result, string action)` and `Entries`.
- Consumed by all command pages.

- [ ] **Step 1: Write the failing log-order test**

```csharp
[Fact]
public void Append_places_the_newest_command_first()
{
    var log = new OperationLogViewModel();
    log.Append(new CommandResult("adb.exe", ["reboot"], 0, "", "", TimeSpan.Zero), "重启设备");
    log.Append(new CommandResult("fastboot.exe", ["devices"], 0, "", "", TimeSpan.Zero), "检测 Fastboot");

    log.Entries[0].Action.Should().Be("检测 Fastboot");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `dotnet test tests/VivoKsu.App.Tests --filter Append_places_the_newest_command_first`

Expected: compilation failure because the log view model is absent.

- [ ] **Step 3: Implement log, package contents, and documentation**

Use an observable collection ordered newest first. Include timestamp, action, executable, arguments, exit code, stdout, stderr, and duration. Configure the project file to copy `platform-tools/**` to output and publish directory. Document tool placement, build, test, publish, first-run behavior, single-device rule, and basic workflows.

- [ ] **Step 4: Run full verification**

Run: `dotnet test VivoKsu.sln`

Run: `dotnet build VivoKsu.sln -c Release`

Run: `dotnet publish src/VivoKsu.App -c Release -r win-x64 --self-contained false -o artifacts\\win-x64`

Expected: all tests pass, the release build succeeds, and publish output contains the executable plus `platform-tools/`.

## Self-Review

- Spec coverage: Tasks 1-2 implement the approved fixed device panel and UI shell; Tasks 3-4 establish reliable Platform Tools execution and discovery; Tasks 5-7 implement every requested capability; Task 8 covers log, packaging, and final verification.
- Placeholder scan: no `TODO`, `TBD`, unspecified error handling, or unbound interfaces remain.
- Type consistency: all pages consume `DeviceSessionViewModel`, `OperationLogViewModel`, and typed services; process execution is exclusively exposed through `ICommandRunner.RunAsync`.
