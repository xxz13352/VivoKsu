# VivoKsu Shell and Overview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the .NET 8 WPF/HandyControl workbench shell with a fixed device state panel, fixed right-side operation log, live device property overview, and the three approved reboot actions.

**Architecture:** Keep `FastbootRsBackend` as the managed/native adapter and extend its typed surface with ADB shell and Fastboot variable reads. Introduce immutable device-detail and log-entry models, then bind a single `MainViewModel` to a three-column window whose central content is an `OverviewView`. Device discovery and property collection run asynchronously; all failures append a structured global log entry instead of replacing the UI.

**Tech Stack:** .NET 8 WPF, CommunityToolkit.Mvvm 8.4.0, HandyControl 3.5.1, xUnit 2.9.3, vendored Rust `fastboot.dll` C ABI.

## Global Constraints

- Target only Windows 10 and Windows 11 with `net8.0-windows`.
- Add `HandyControl` version `3.5.1`; its default theme must not determine the product visuals.
- Retain the `FastbootRsBackend` boundary: no view model or view can call `FastbootRsNative` directly.
- Device overview contains only device information plus normal, Bootloader, and Fastboot reboot actions.
- The lower-left device state is permanent. The operation log is a permanent right-side panel and is the only shared log surface.
- Use graphite/titanium surfaces, signal orange for mode-changing actions, and low-saturation aqua only for healthy transport/verification.
- Do not add Windows 7 support, arbitrary partition flashing, ROOT execution, or Vivo line-flash execution in this plan.

---

## File Structure

```text
src/VivoKsu.App/
├── App.xaml                                  # Theme dictionaries and HandyControl resources
├── App.xaml.cs                               # Composition root and startup discovery
├── MainWindow.xaml                           # Stable top/left/main/right frame
├── MainWindow.xaml.cs                        # Receives the composed MainViewModel
├── Models/
│   ├── AppPage.cs                            # Expanded navigation identifiers
│   ├── DeviceDetailsSnapshot.cs              # Immutable property matrix data
│   ├── DeviceSnapshot.cs                     # Existing transport summary
│   ├── OperationKind.cs                      # Adds Mirroring for later pages
│   ├── OperationLogEntry.cs                  # Immutable global log record
│   └── OperationLogLevel.cs                  # Info/Success/Warning/Error
├── Services/
│   ├── DeviceInfoService.cs                  # Reads ADB shell and Fastboot getvar values
│   ├── FastbootRsBackend.cs                  # Typed async shell/getvar/reboot methods
│   ├── FastbootRsNative.cs                   # C ABI declarations for adb_shell/getvar
│   ├── IFastbootRsNativeApi.cs               # Testable native API contract
│   ├── NativeFastbootRsApi.cs                # UTF-8 buffer marshaling implementation
│   └── OperationLogService.cs                # Appends globally ordered operation log entries
├── ViewModels/
│   ├── DeviceSessionViewModel.cs             # Transport summary plus DeviceDetailsSnapshot
│   ├── MainViewModel.cs                      # Navigation, refresh, shared logs and overview
│   ├── OperationLogViewModel.cs              # Observable global log projection
│   └── OverviewViewModel.cs                  # Reboot commands and enabled-state rules
└── Views/
    ├── OverviewView.xaml                     # Device matrix and three restart actions
    └── OverviewView.xaml.cs

tests/VivoKsu.App.Tests/
├── DeviceInfoServiceTests.cs
├── DeviceSessionViewModelTests.cs
├── FastbootRsBackendTests.cs
├── MainViewModelTests.cs
├── NativeCommandContractTests.cs
├── OperationLogServiceTests.cs
└── OverviewViewModelTests.cs
```

## Task 1: Add the UI dependency and product resource system

**Files:**
- Modify: `src/VivoKsu.App/VivoKsu.App.csproj`
- Modify: `src/VivoKsu.App/App.xaml`
- Modify: `src/VivoKsu.App/MainWindow.xaml`
- Test: `tests/VivoKsu.App/VivoKsu.App.Tests.csproj`

**Interfaces:**
- Consumes: existing WPF application project and `CommunityToolkit.Mvvm` package.
- Produces: `HandyControl` 3.5.1 resources, named application brushes/styles, and a three-column shell ready to host the later view models.

- [ ] **Step 1: Add the dependency and confirm the old project still builds**

Add this package reference alongside `CommunityToolkit.Mvvm`:

```xml
<PackageReference Include="HandyControl" Version="3.5.1" />
```

Run: `dotnet build VivoKsu.slnx -c Release`

Expected: build succeeds with no package compatibility warning.

- [ ] **Step 2: Replace the temporary teal resource set with the workbench tokens**

In `App.xaml`, define resources with these exact base colors:

```xml
<Color x:Key="InkColor">#101213</Color>
<Color x:Key="RailColor">#151819</Color>
<Color x:Key="SurfaceColor">#1A1E1F</Color>
<Color x:Key="EdgeColor">#343A3D</Color>
<Color x:Key="TextColor">#EEF0ED</Color>
<Color x:Key="MutedColor">#9BA4A5</Color>
<Color x:Key="SignalOrangeColor">#DC6B43</Color>
<Color x:Key="ConnectionAquaColor">#83C8C3</Color>
```

Create `SolidColorBrush` resources from each color and styles named `WorkbenchButtonStyle`, `SignalButtonStyle`, `NavigationButtonStyle`, and `LogTextStyle`. Keep corners at 3-5px and do not add gradient brushes.

- [ ] **Step 3: Replace the single scrollable body with shell grid tracks**

Make `MainWindow.xaml` use this structural layout:

```xml
<Grid>
  <Grid.RowDefinitions><RowDefinition Height="58"/><RowDefinition Height="*"/></Grid.RowDefinitions>
  <Grid.ColumnDefinitions>
    <ColumnDefinition Width="210"/>
    <ColumnDefinition Width="*"/>
    <ColumnDefinition Width="342"/>
  </Grid.ColumnDefinitions>
  <!-- top bar spans all columns; rail/main/log start in row 1 -->
</Grid>
```

Leave central page content as a temporary `ContentControl` bound to `SelectedPage`; add permanent placeholders for the left device state and the right log panel. Do not move the log into a navigation destination.

- [ ] **Step 4: Build and visually smoke-test the shell**

Run: `dotnet build VivoKsu.slnx -c Release`

Run: `Start-Process .\src\VivoKsu.App\bin\Release\net8.0-windows\VivoKsu.App.exe`

Expected: a 58px top bar, 210px rail, center work area, and 342px right panel are visible without horizontal clipping at the existing minimum window size.

## Task 2: Model detailed device data and global operation logs

**Files:**
- Create: `src/VivoKsu.App/Models/DeviceDetailsSnapshot.cs`
- Create: `src/VivoKsu.App/Models/OperationLogEntry.cs`
- Create: `src/VivoKsu.App/Models/OperationLogLevel.cs`
- Create: `src/VivoKsu.App/Services/OperationLogService.cs`
- Create: `src/VivoKsu.App/ViewModels/OperationLogViewModel.cs`
- Modify: `src/VivoKsu.App/Models/OperationKind.cs`
- Test: `tests/VivoKsu.App.Tests/OperationLogServiceTests.cs`

**Interfaces:**
- Consumes: `ObservableCollection<T>` and `OperationKind`.
- Produces: `DeviceDetailsSnapshot`, `OperationLogEntry`, `OperationLogService.Write`, and `OperationLogViewModel.Entries` for later session and UI tasks.

- [ ] **Step 1: Write failing log-order and bounded-list tests**

Create `OperationLogServiceTests.cs` with these assertions:

```csharp
[Fact]
public void Write_appends_entries_in_timestamp_order_and_keeps_the_newest_500()
{
    var service = new OperationLogService();
    for (var index = 0; index < 501; index++)
        service.Write(OperationLogLevel.Info, $"line-{index}");

    Assert.Equal(500, service.Entries.Count);
    Assert.Equal("line-1", service.Entries[0].Message);
    Assert.Equal("line-500", service.Entries[^1].Message);
}
```

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~OperationLogServiceTests`

Expected: FAIL because `OperationLogService` does not exist.

- [ ] **Step 2: Implement immutable records and the bounded log store**

Create these public contracts:

```csharp
public enum OperationLogLevel { Info, Success, Warning, Error }
public sealed record OperationLogEntry(DateTimeOffset Timestamp, OperationLogLevel Level, string Message, string? OperationId = null);
public sealed record DeviceDetailsSnapshot(
    string Brand, string Model, string Codename, string Serial,
    string AndroidVersion, string FirmwareVersion, string KernelVersion,
    string ActiveSlot, string BootloaderState, string VerifiedBootState,
    string UsbDebuggingState, string BuildTime)
{
    public static DeviceDetailsSnapshot Empty { get; } = new("--", "未检测到设备", "--", "--", "--", "--", "--", "--", "--", "--", "--", "--");
}
```

`OperationLogService` owns `ObservableCollection<OperationLogEntry> Entries`, appends on the UI thread, and removes index 0 while the count exceeds 500. `OperationLogViewModel` exposes the same collection as a read-only bindable property.

- [ ] **Step 3: Extend the operation enum without changing existing values**

Add `Mirroring` after `Flashing` in `OperationKind`. Keep all existing enum members to preserve the current tests and future feature state transitions.

- [ ] **Step 4: Run the focused and complete test suites**

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~OperationLogServiceTests`

Expected: PASS.

Run: `dotnet test VivoKsu.slnx`

Expected: PASS with all existing tests retained.

## Task 3: Extend the native adapter for property reads

**Files:**
- Modify: `src/VivoKsu.App/Services/FastbootRsNative.cs`
- Modify: `src/VivoKsu.App/Services/IFastbootRsNativeApi.cs`
- Modify: `src/VivoKsu.App/Services/NativeFastbootRsApi.cs`
- Modify: `src/VivoKsu.App/Services/FastbootRsBackend.cs`
- Test: `tests/VivoKsu.App.Tests/NativeCommandContractTests.cs`
- Test: `tests/VivoKsu.App.Tests/FastbootRsBackendTests.cs`

**Interfaces:**
- Consumes: `fastboot.dll` exports `adb_shell(serial, command, outBuf, length)` and `fastboot_getvar(serial, variable, outBuf, length)`.
- Produces: `IFastbootRsNativeApi.Shell`, `IFastbootRsNativeApi.GetVar`, `FastbootRsBackend.ShellAsync`, and `FastbootRsBackend.GetVarAsync`.

- [ ] **Step 1: Write failing adapter delegation tests**

Add this test to `FastbootRsBackendTests.cs`:

```csharp
[Fact]
public async Task ShellAsync_forwards_the_serial_and_command()
{
    var api = new FakeNativeApi("A1B2\tdevice\n") { ShellResult = "ro.product.model=iQOO 12" };
    var backend = new FastbootRsBackend(api);

    var output = await backend.ShellAsync("A1B2", "getprop ro.product.model", CancellationToken.None);

    Assert.Equal("ro.product.model=iQOO 12", output);
    Assert.Equal(("A1B2", "getprop ro.product.model"), api.LastShellRequest);
}
```

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~FastbootRsBackendTests`

Expected: FAIL because `ShellAsync` and the fake API members do not exist.

- [ ] **Step 2: Declare the two C ABI functions and expose testable API methods**

Add these declarations to `FastbootRsNative.cs`:

```csharp
[DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
internal static extern int adb_shell(string? serial, string command, IntPtr outputBuffer, nuint bufferLength);

[DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
internal static extern int fastboot_getvar(string? serial, string variable, IntPtr outputBuffer, nuint bufferLength);
```

Add `string Shell(string? serial, string command)` and `string GetVar(string? serial, string variable)` to `IFastbootRsNativeApi` and implement both in `NativeFastbootRsApi` through its existing UTF-8 `ReadBuffer` helper.

- [ ] **Step 3: Add cancellation-aware backend methods**

Add the following methods to `FastbootRsBackend`:

```csharp
public Task<string> ShellAsync(string? serial, string command, CancellationToken cancellationToken) =>
    Task.Run(() => nativeApi.Shell(serial, command), cancellationToken);

public Task<string> GetVarAsync(string? serial, string variable, CancellationToken cancellationToken) =>
    Task.Run(() => nativeApi.GetVar(serial, variable), cancellationToken);
```

Update every test fake to implement `Shell` and `GetVar` with deterministic defaults.

- [ ] **Step 4: Run adapter tests**

Run: `dotnet test VivoKsu.slnx --filter "FullyQualifiedName~FastbootRsBackendTests|FullyQualifiedName~NativeCommandContractTests"`

Expected: PASS. The test suite must compile with every existing fake native API updated.

## Task 4: Parse live device details and coordinate overview refresh/restarts

**Files:**
- Create: `src/VivoKsu.App/Services/DeviceInfoService.cs`
- Create: `src/VivoKsu.App/ViewModels/OverviewViewModel.cs`
- Modify: `src/VivoKsu.App/ViewModels/DeviceSessionViewModel.cs`
- Modify: `src/VivoKsu.App/ViewModels/MainViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/DeviceInfoServiceTests.cs`
- Test: `tests/VivoKsu.App.Tests/DeviceSessionViewModelTests.cs`
- Test: `tests/VivoKsu.App.Tests/OverviewViewModelTests.cs`

**Interfaces:**
- Consumes: `FastbootRsBackend.ShellAsync/GetVarAsync/RebootAsync`, `DeviceDetailsSnapshot`, and `OperationLogService`.
- Produces: `DeviceInfoService.ReadAdbAsync`, `DeviceInfoService.ReadFastbootAsync`, `DeviceSessionViewModel.Details`, and the three overview commands.

- [ ] **Step 1: Write a failing property parser test**

Create `DeviceInfoServiceTests.cs` with the ADB fixture:

```csharp
const string properties = "[ro.product.brand]: [vivo]\n[ro.product.model]: [V2318A]\n[ro.product.device]: [PD2307]\n[ro.build.version.release]: [15]\n[ro.build.display.id]: [PD2307_A_15.0.12.1.W10]\n[ro.build.version.incremental]: [15.0.12.1]\n";
```

Assert that `ReadAdbAsync("RF8", CancellationToken.None)` returns brand `vivo`, model `V2318A`, codename `PD2307`, Android version `15`, and firmware version `PD2307_A_15.0.12.1.W10` when the fake shell API returns the fixture.

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~DeviceInfoServiceTests`

Expected: FAIL because `DeviceInfoService` does not exist.

- [ ] **Step 2: Implement deterministic ADB and Fastboot property collection**

`ReadAdbAsync` runs one shell command for each source: `getprop`, `uname -r`, `getprop ro.build.date.utc`, `getprop ro.boot.slot_suffix`, and `getprop ro.boot.verifiedbootstate`. Parse bracketed getprop rows by key, convert `_a`/`_b` to `a`/`b`, and convert Unix build seconds using `DateTimeOffset.FromUnixTimeSeconds`.

`ReadFastbootAsync` reads `current-slot` and `unlocked` through `GetVarAsync`; it overwrites only values that Fastboot reports successfully. Any individual failed read is logged as `Warning` and leaves the corresponding field as `Not available`.

- [ ] **Step 3: Expand session state and implement restart command policy**

Add `DeviceDetailsSnapshot Details { get; }` to `DeviceSessionViewModel` and an `ApplyDetails(DeviceDetailsSnapshot details)` method. Do not replace the existing transport summary fields.

`OverviewViewModel` has these commands and targets:

```csharp
IAsyncRelayCommand RebootSystemCommand    // native target: ""
IAsyncRelayCommand RebootBootloaderCommand // native target: "bootloader"
IAsyncRelayCommand RebootFastbootCommand   // native target: "fastboot"
```

Each command checks that `DeviceSession.ConnectionState == DeviceConnectionState.AdbConnected`, calls `BeginOperation(OperationKind.Rebooting, ...)`, writes Info/Success or Error log entries, and always returns the session to a non-busy completed/failed state.

- [ ] **Step 4: Verify the parser, session binding, and reboot target tests**

Run: `dotnet test VivoKsu.slnx --filter "FullyQualifiedName~DeviceInfoServiceTests|FullyQualifiedName~DeviceSessionViewModelTests|FullyQualifiedName~OverviewViewModelTests"`

Expected: PASS with assertions for `""`, `"bootloader"`, and `"fastboot"` targets.

## Task 5: Build the permanent log/overview UI and compose the application

**Files:**
- Create: `src/VivoKsu.App/Views/OverviewView.xaml`
- Create: `src/VivoKsu.App/Views/OverviewView.xaml.cs`
- Modify: `src/VivoKsu.App/MainWindow.xaml`
- Modify: `src/VivoKsu.App/MainWindow.xaml.cs`
- Modify: `src/VivoKsu.App/App.xaml.cs`
- Modify: `src/VivoKsu.App/ViewModels/MainViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/MainViewModelTests.cs`

**Interfaces:**
- Consumes: `OverviewViewModel`, `DeviceSessionViewModel`, `OperationLogViewModel`, and `AppPage`.
- Produces: a data-templated overview in the center frame and fixed bindings for left device state and right log entries.

- [ ] **Step 1: Write a failing navigation/menu test**

Replace the existing page test with assertions for all new routes:

```csharp
[Theory]
[InlineData(AppPage.QuickFlash)]
[InlineData(AppPage.AdbMirror)]
[InlineData(AppPage.RootTools)]
[InlineData(AppPage.FileManager)]
[InlineData(AppPage.LineFlash)]
public void Selecting_a_workspace_page_updates_the_current_page(AppPage page)
{
    var viewModel = TestMainViewModelFactory.Create();
    viewModel.SelectPageCommand.Execute(page);
    Assert.Equal(page, viewModel.SelectedPage);
}
```

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~MainViewModelTests`

Expected: FAIL until `AppPage` and the factory constructor are expanded.

- [ ] **Step 2: Define the new navigation identifiers and shared MainViewModel ownership**

Replace the old `AdbActions`, `FileTransfer`, `FastbootFlash`, and `OperationLog` menu model with:

```csharp
public enum AppPage { Overview, QuickFlash, AdbMirror, RootTools, FileManager, LineFlash }
```

`MainViewModel` exposes `DeviceSession`, `Overview`, `Logs`, `RefreshDeviceCommand`, and `SelectPageCommand`. `RefreshDeviceCommand` calls backend discovery then `DeviceInfoService`, updates the session on the dispatcher, and writes every discovery stage to the shared log.

- [ ] **Step 3: Implement OverviewView with no unrelated actions**

Use `OverviewView.xaml` to render:

```text
identity strip: device figure, model, codename, serial, active slot, bootloader, kernel, build
property matrix: brand/model, codename, Android, firmware, kernel, build time, slot, verified boot, USB debugging
restart row: normal system, Bootloader, Fastboot
```

Bind the only three executable buttons to `Overview.RebootSystemCommand`, `Overview.RebootBootloaderCommand`, and `Overview.RebootFastbootCommand`. The view must not contain flash, projection, ROOT, file, or line-flash controls.

- [ ] **Step 4: Bind permanent rail and log regions**

In `MainWindow.xaml`, bind the left-bottom panel to `DeviceSession.DeviceName`, `Serial`, `ConnectionLabel`, `Details.ActiveSlot`, `AndroidVersion`, `BatteryLevel`, and `StatusText`.

Bind the right `ListBox`/`ItemsControl` to `Logs.Entries`; render timestamp, level, and message in monospace. Bind `SelectedPage == AppPage.Overview` to the center `OverviewView`; show plain non-executable placeholders for the remaining four feature pages.

- [ ] **Step 5: Compose real services at application startup**

In `App.OnStartup`, create `NativeFastbootRsApi`, `FastbootRsBackend`, `OperationLogService`, `OperationLogViewModel`, `DeviceSessionViewModel`, `DeviceInfoService`, `OverviewViewModel`, and `MainViewModel`. Pass the composed `MainViewModel` to `MainWindow`; catch missing DLL exceptions during the initial refresh, leave navigation visible, and write an Error entry with the expected library path.

- [ ] **Step 6: Run regression, build, and visual checks**

Run: `dotnet test VivoKsu.slnx`

Expected: PASS.

Run: `dotnet build VivoKsu.slnx -c Release`

Expected: zero warnings and zero errors.

Run: `Start-Process .\src\VivoKsu.App\bin\Release\net8.0-windows\VivoKsu.App.exe`

Visual acceptance: overview shows only device information and three restart actions; the device panel remains at lower left; the log panel stays at right while navigating; text does not overlap at the minimum window size.

## Task 6: Record the first-phase handoff and remove obsolete UI test debris

**Files:**
- Modify: `tests/VivoKsu.App.Tests/UnitTest1.cs`
- Modify: `docs/superpowers/specs/2026-08-10-vivoksu-device-tool-design.md`

**Interfaces:**
- Consumes: completed shell and overview test coverage.
- Produces: an accurate documented state before the Quick Flash implementation plan begins.

- [ ] **Step 1: Replace the empty generated test class with a concrete regression or remove it**

Delete `UnitTest1.cs` only after confirming all meaningful tests named in Tasks 2-5 exist and pass. Do not leave an empty test file.

- [ ] **Step 2: Mark first-increment delivery in the design document**

Add a dated implementation note under `Delivery Order` stating that item 1 and item 2 are complete only after the checks below pass. Do not mark Quick Flash or later items complete.

- [ ] **Step 3: Execute the final first-phase verification**

Run: `dotnet test VivoKsu.slnx`

Run: `dotnet build VivoKsu.slnx -c Release`

Expected: every test passes and the Release build has zero warnings/errors.

- [ ] **Step 4: Commit when repository metadata exists**

This workspace currently has no `.git` directory. Do not initialize a repository as part of the feature work. If Git metadata exists by the time of implementation, use:

```powershell
git add src/VivoKsu.App tests/VivoKsu.App.Tests docs/superpowers
git commit -m "feat: add VivoKsu workbench shell and device overview"
```

## Plan Self-Review

- Spec coverage: Tasks 1-5 implement the first delivery increment, the full device information matrix, exactly three reboot actions, permanent right log, permanent lower-left state, validated HandyControl integration, and Windows 10/11-only baseline. Quick Flash, mirror, file manager, ROOT, and line flash are intentionally deferred to dedicated implementation plans as required by the delivery order.
- Placeholder scan: no implementation step relies on an unspecified API; native function names, model constructors, command targets, file paths, and test commands are declared in this plan.
- Type consistency: `DeviceDetailsSnapshot`, `OperationLogEntry`, `OperationLogLevel`, `OperationLogService`, `FastbootRsBackend.ShellAsync`, `FastbootRsBackend.GetVarAsync`, `OverviewViewModel`, and the new `AppPage` values are introduced before later tasks consume them.
