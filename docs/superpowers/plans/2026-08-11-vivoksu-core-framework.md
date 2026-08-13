# VivoKsu Core Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a single-device operation framework that serializes foreground work, pauses automatic discovery during tasks, centralizes composition, and migrates reboot and quick-flash workflows first.

**Architecture:** `OperationCoordinator` owns one cancellable foreground operation and projects its state to the existing `DeviceSessionViewModel` and shared log. `DeviceMonitorService` owns automatic refresh and uses the coordinator as its discovery gate. `AppComposition` becomes the composition root so `MainWindow` receives a ready-made `MainViewModel` and only handles window behavior.

**Tech Stack:** .NET 8 WPF, C# 12, CommunityToolkit.Mvvm 8.4.0, xUnit, existing `FastbootRsBackend` native boundary.

## Global Constraints

- Keep `FastbootRsBackend` as the sole managed/native ADB/Fastboot boundary.
- Target Windows 10/11 x64 and preserve bundled `platform-tools`, scrcpy, APK, and ROOT resources.
- Keep the current UI layout unchanged, including the fixed lower-left device status and fixed right-side log pane.
- Support one connected device and one foreground device-changing operation at a time.
- Automatic discovery must skip while a foreground operation is active and preserve the existing two-consecutive-empty-discovery disconnect rule.
- Preserve the existing `DeviceSessionViewModel` public state methods and existing view-model constructor compatibility unless a test is deliberately updated in the same task.
- Do not introduce a new UI library, Windows 7 support, arbitrary partition flashing, or a different ROOT patching algorithm.

---

## File Structure

```text
src/VivoKsu.App/
├── Models/
│   └── OperationStateSnapshot.cs          # Immutable global operation state
├── Services/
│   ├── IDeviceRefreshService.cs           # Testable session-refresh contract
│   ├── IDeviceMonitor.cs                  # Small monitor contract for MainViewModel
│   ├── IOperationCoordinator.cs           # Foreground task contract
│   ├── OperationContext.cs                # Stage/progress reporter for task delegates
│   ├── OperationCoordinator.cs            # Serial, cancellable task lifecycle
│   ├── DeviceMonitorService.cs             # Timed and manual refresh coordinator
│   └── AppComposition.cs                  # Application composition and shutdown root
├── App.xaml                               # Removes StartupUri for explicit composition
├── App.xaml.cs                            # Builds and stops AppComposition
├── MainWindow.xaml.cs                     # Receives the composed app host
└── ViewModels/
    ├── MainViewModel.cs                   # Delegates refresh to DeviceMonitorService
    ├── OverviewViewModel.cs               # Routes reboot through coordinator
    └── QuickFlashViewModel.cs             # Routes flash through coordinator

tests/VivoKsu.App.Tests/
├── OperationCoordinatorTests.cs
├── DeviceMonitorServiceTests.cs
├── AppCompositionTests.cs
├── OverviewViewModelTests.cs
├── QuickFlashViewModelTests.cs
└── MainViewModelTests.cs
```

## Task 1: Add immutable operation state and the serial task coordinator

**Files:**
- Create: `src/VivoKsu.App/Models/OperationStateSnapshot.cs`
- Create: `src/VivoKsu.App/Services/IOperationCoordinator.cs`
- Create: `src/VivoKsu.App/Services/OperationContext.cs`
- Create: `src/VivoKsu.App/Services/OperationCoordinator.cs`
- Test: `tests/VivoKsu.App.Tests/OperationCoordinatorTests.cs`

**Interfaces:**
- Consumes: `DeviceSessionViewModel.BeginOperation`, `CompleteOperation`, `CancelOperation`, `FailOperation`; `OperationLogService.Write`; `OperationKind` and `OperationLogLevel`.
- Produces: `IOperationCoordinator.RunAsync`, `CancelCurrent`, `IsBusy`, `State`, and `StateChanged`; `OperationContext.ReportStage` and `ReportProgress`.

- [ ] **Step 1: Write the failing coordinator tests**

Create `OperationCoordinatorTests.cs` with these tests and a `CreateCoordinator` helper that constructs a new `DeviceSessionViewModel`, `OperationLogService`, and `OperationCoordinator`:

```csharp
[Fact]
public async Task RunAsync_serializes_concurrent_operations_and_restores_idle_state()
{
    var (coordinator, session, _) = CreateCoordinator();
    var firstEntered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
    var releaseFirst = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
    var secondEntered = false;

    var first = coordinator.RunAsync(OperationKind.Flashing, "正在刷写 boot", async (_, token) =>
    {
        firstEntered.SetResult(true);
        await releaseFirst.Task.WaitAsync(token);
    });
    await firstEntered.Task.WaitAsync(TimeSpan.FromSeconds(2));
    var second = coordinator.RunAsync(OperationKind.Rebooting, "正在重启", (_, _) =>
    {
        secondEntered = true;
        return Task.CompletedTask;
    });

    Assert.True(coordinator.IsBusy);
    Assert.False(secondEntered);
    releaseFirst.SetResult(true);
    await Task.WhenAll(first, second);

    Assert.True(secondEntered);
    Assert.False(coordinator.IsBusy);
    Assert.Equal(OperationKind.Completed, session.OperationKind);
}

[Fact]
public async Task CancelCurrent_cancels_the_active_delegate_and_records_a_warning()
{
    var (coordinator, session, logs) = CreateCoordinator();
    var entered = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
    var operation = coordinator.RunAsync(OperationKind.Flashing, "正在刷写 boot", async (_, token) =>
    {
        entered.SetResult(true);
        await Task.Delay(Timeout.InfiniteTimeSpan, token);
    });
    await entered.Task.WaitAsync(TimeSpan.FromSeconds(2));

    coordinator.CancelCurrent();
    await Assert.ThrowsAnyAsync<OperationCanceledException>(() => operation);

    Assert.False(coordinator.IsBusy);
    Assert.Equal(OperationKind.Canceled, session.OperationKind);
    Assert.Contains(logs.Entries, entry => entry.Level == OperationLogLevel.Warning && entry.OperationId is not null);
}

[Fact]
public async Task ReportStage_updates_session_state_snapshot_and_correlated_log()
{
    var (coordinator, session, logs) = CreateCoordinator();
    await coordinator.RunAsync(OperationKind.Transferring, "正在传输文件", (context, _) =>
    {
        context.ReportStage("正在上传 boot.img");
        context.ReportProgress(0.5);
        return Task.CompletedTask;
    });

    Assert.Equal("操作完成", session.StatusText);
    Assert.Equal(OperationKind.Idle, coordinator.State.Kind);
    Assert.Contains(logs.Entries, entry => entry.Message == "正在上传 boot.img" && entry.OperationId is not null);
}
```

- [ ] **Step 2: Run the coordinator tests and verify failure**

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~OperationCoordinatorTests`

Expected: FAIL because `OperationCoordinator`, `OperationContext`, and `OperationStateSnapshot` do not exist.

- [ ] **Step 3: Add the immutable operation state contracts**

Create `OperationStateSnapshot.cs`:

```csharp
namespace VivoKsu.App.Models;

public sealed record OperationStateSnapshot(
    OperationKind Kind,
    string? OperationId,
    string Title,
    string Stage,
    double? Progress,
    DateTimeOffset? StartedAt,
    bool IsCancellable)
{
    public static OperationStateSnapshot Idle { get; } = new(
        OperationKind.Idle, null, "", "", null, null, false);
}
```

Create `IOperationCoordinator.cs`:

```csharp
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public interface IOperationCoordinator
{
    bool IsBusy { get; }
    OperationStateSnapshot State { get; }
    event EventHandler? StateChanged;
    Task RunAsync(OperationKind kind, string title,
        Func<OperationContext, CancellationToken, Task> operation,
        CancellationToken cancellationToken = default);
    void CancelCurrent();
}
```

Create `OperationContext.cs` so it stores the operation ID and calls an injected `Action<string, double?>` from `ReportStage` and `ReportProgress`. `ReportProgress` must reject values below `0` and above `1` with `ArgumentOutOfRangeException`.

- [ ] **Step 4: Implement `OperationCoordinator` lifecycle**

Implement `OperationCoordinator` with `SemaphoreSlim(1, 1)`, a private synchronization lock, the shared session, and shared log service. Its `RunAsync` must:

```csharp
await operationGate.WaitAsync(cancellationToken);
using var linkedCancellation = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
var operationId = Guid.NewGuid().ToString("N");
SetCurrent(kind, operationId, title, title, null, linkedCancellation);
session.BeginOperation(kind, title);
logs.Write(OperationLogLevel.Info, title, operationId);
try
{
    await operation(new OperationContext(operationId, Report), linkedCancellation.Token);
    session.CompleteOperation();
    logs.Write(OperationLogLevel.Success, $"{title}完成。", operationId);
}
catch (OperationCanceledException) when (linkedCancellation.IsCancellationRequested)
{
    session.CancelOperation();
    logs.Write(OperationLogLevel.Warning, $"{title}已取消。", operationId);
    throw;
}
catch (Exception exception)
{
    session.FailOperation($"{title}失败");
    logs.Write(OperationLogLevel.Error, exception.Message, operationId);
    throw;
}
finally
{
    ClearCurrent(linkedCancellation);
    operationGate.Release();
}
```

`Report` must update `State`, call `session.BeginOperation(State.Kind, stage)`, write the stage to the log with the same operation ID, and raise `StateChanged`. `CancelCurrent` must only cancel the current linked token source; it must not release the semaphore or modify session state itself.

- [ ] **Step 5: Run the coordinator tests and full regression**

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~OperationCoordinatorTests`

Expected: PASS.

Run: `dotnet test VivoKsu.slnx`

Expected: existing tests still pass because no current view model uses the new coordinator.

- [ ] **Step 6: Commit when repository metadata exists**

Run: `git rev-parse --is-inside-work-tree`

If it returns `true`:

```powershell
git add src/VivoKsu.App/Models/OperationStateSnapshot.cs src/VivoKsu.App/Services/IOperationCoordinator.cs src/VivoKsu.App/Services/OperationContext.cs src/VivoKsu.App/Services/OperationCoordinator.cs tests/VivoKsu.App.Tests/OperationCoordinatorTests.cs
git commit -m "feat: add serialized device operation coordinator"
```

If the command fails, keep the verified changes uncommitted; do not initialize a repository.

## Task 2: Add a coordinated asynchronous device monitor

**Files:**
- Create: `src/VivoKsu.App/Services/IDeviceRefreshService.cs`
- Create: `src/VivoKsu.App/Services/DeviceMonitorService.cs`
- Modify: `src/VivoKsu.App/Services/DeviceSessionService.cs`
- Test: `tests/VivoKsu.App.Tests/DeviceMonitorServiceTests.cs`

**Interfaces:**
- Consumes: `IOperationCoordinator.IsBusy`, `StateChanged`, `DeviceSessionViewModel`, `DeviceRefreshMode`, and existing `DeviceSessionService.RefreshAsync` behavior.
- Produces: `DeviceMonitorService.StartAsync`, `StopAsync`, `RefreshManualAsync`, `RefreshAutomaticallyAsync`, `DeviceRefreshed`, and a testable `IDeviceRefreshService` contract.

- [ ] **Step 1: Write failing monitor tests**

Create `DeviceMonitorServiceTests.cs` with a fake `IDeviceRefreshService` that increments `CallCount`, records modes, and returns `Task.CompletedTask`:

```csharp
[Fact]
public async Task RefreshAutomaticallyAsync_skips_discovery_while_a_foreground_operation_is_running()
{
    var (coordinator, session, _) = CreateCoordinator();
    var refresh = new RecordingRefreshService();
    var monitor = new DeviceMonitorService(refresh, session, coordinator, TimeSpan.FromHours(1));
    var release = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
    var operation = coordinator.RunAsync(OperationKind.Flashing, "正在刷写", (_, token) => release.Task.WaitAsync(token));
    await WaitUntilAsync(() => coordinator.IsBusy);

    await monitor.RefreshAutomaticallyAsync();

    Assert.Equal(0, refresh.CallCount);
    release.SetResult(true);
    await operation;
}

[Fact]
public async Task Completing_a_foreground_operation_triggers_one_compensating_automatic_refresh()
{
    var (coordinator, session, _) = CreateCoordinator();
    var refresh = new RecordingRefreshService();
    var monitor = new DeviceMonitorService(refresh, session, coordinator, TimeSpan.FromHours(1));

    await coordinator.RunAsync(OperationKind.Rebooting, "正在重启", (_, _) => Task.CompletedTask);
    await WaitUntilAsync(() => refresh.CallCount == 1);

    Assert.Equal([DeviceRefreshMode.Automatic], refresh.Modes);
}

[Fact]
public async Task RefreshManualAsync_and_automatic_refresh_share_one_refresh_gate()
{
    var (coordinator, session, _) = CreateCoordinator();
    var refresh = new BlockingRefreshService();
    var monitor = new DeviceMonitorService(refresh, session, coordinator, TimeSpan.FromHours(1));
    var manual = monitor.RefreshManualAsync(logActivity: true);
    await refresh.Entered.Task.WaitAsync(TimeSpan.FromSeconds(2));

    await monitor.RefreshAutomaticallyAsync();

    Assert.Equal(1, refresh.CallCount);
    refresh.Release.SetResult(true);
    await manual;
}
```

`WaitUntilAsync` loops with `Task.Delay(10)` for at most two seconds and throws `TimeoutException` when the predicate remains false.

- [ ] **Step 2: Run monitor tests and verify failure**

Run: `dotnet test VivoKsu.slnx --filter FullyQualifiedName~DeviceMonitorServiceTests`

Expected: FAIL because `IDeviceRefreshService` and `DeviceMonitorService` do not exist.

- [ ] **Step 3: Add the refresh abstraction without changing semantics**

Create `IDeviceRefreshService.cs`:

```csharp
using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public interface IDeviceRefreshService
{
    Task RefreshAsync(DeviceSessionViewModel session, CancellationToken cancellationToken,
        bool logActivity = true, DeviceRefreshMode refreshMode = DeviceRefreshMode.Manual);
}
```

Modify the declaration of `DeviceSessionService` to implement `IDeviceRefreshService`; do not change its method signature or its two-empty-disconnect implementation.

- [ ] **Step 4: Implement `DeviceMonitorService`**

Create a service with constructor:

```csharp
public DeviceMonitorService(
    IDeviceRefreshService refreshService,
    DeviceSessionViewModel session,
    IOperationCoordinator coordinator,
    TimeSpan? interval = null,
    SynchronizationContext? synchronizationContext = null)
```

Use a private `SemaphoreSlim refreshGate = new(1, 1)`, default interval `TimeSpan.FromSeconds(3)`, a lifecycle `CancellationTokenSource`, and `Task? monitorTask`. Add this event:

```csharp
public event Func<DeviceRefreshMode, CancellationToken, Task>? DeviceRefreshed;
```

Implement `RefreshManualAsync(bool logActivity, CancellationToken)` by calling `RefreshCoreAsync(logActivity, DeviceRefreshMode.Manual, cancellationToken)`. Implement `RefreshAutomaticallyAsync` with `logActivity: false` and automatic mode. `RefreshCoreAsync` must return immediately when the coordinator is busy, when `refreshGate.WaitAsync(0, cancellationToken)` cannot acquire the gate, or when the lifecycle token is cancelled. Otherwise invoke `refreshService.RefreshAsync`, then await every subscriber to `DeviceRefreshed` in invocation-list order.

Subscribe to `coordinator.StateChanged`; when the coordinator transitions from busy to idle, schedule exactly one `RefreshAutomaticallyAsync` task using an `Interlocked` flag. Capture the optional synchronization context at construction and use a `TaskCompletionSource` plus `Post` to run session-mutating refresh work on that context; if it is null or already current, execute inline.

`StartAsync` creates the lifecycle token source once and starts a `PeriodicTimer` loop. `StopAsync` cancels the loop, awaits it, unsubscribes from `StateChanged`, and disposes resources. The loop catches `OperationCanceledException` only for its own lifecycle token and records unexpected exceptions as Error logs.

- [ ] **Step 5: Run monitor and existing session tests**

Run: `dotnet test VivoKsu.slnx --filter "FullyQualifiedName~DeviceMonitorServiceTests|FullyQualifiedName~DeviceSessionServiceTests"`

Expected: PASS. Existing disconnect delay and busy-session behavior remain unchanged.

- [ ] **Step 6: Commit when repository metadata exists**

```powershell
git add src/VivoKsu.App/Services/IDeviceRefreshService.cs src/VivoKsu.App/Services/DeviceMonitorService.cs src/VivoKsu.App/Services/DeviceSessionService.cs tests/VivoKsu.App.Tests/DeviceMonitorServiceTests.cs
git commit -m "feat: coordinate background device monitoring"
```

Run the commands only after `git rev-parse --is-inside-work-tree` succeeds.

## Task 3: Move composition and monitor lifetime out of MainWindow

**Files:**
- Create: `src/VivoKsu.App/Services/AppComposition.cs`
- Modify: `src/VivoKsu.App/App.xaml`
- Modify: `src/VivoKsu.App/App.xaml.cs`
- Modify: `src/VivoKsu.App/MainWindow.xaml.cs`
- Modify: `src/VivoKsu.App/ViewModels/MainViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/AppCompositionTests.cs`
- Test: `tests/VivoKsu.App.Tests/MainViewModelTests.cs`

**Interfaces:**
- Consumes: `FastbootRsApiFactory.CreateDefault`, `FastbootRsBackend`, all existing feature services/view models, `OperationCoordinator`, and `DeviceMonitorService` from Tasks 1-2.
- Produces: `AppComposition.CreateDefault`, `MainViewModel` constructed with a monitor, explicit WPF startup/shutdown, and no `DispatcherTimer` or dependency construction in `MainWindow`.

- [ ] **Step 1: Write failing composition and main-view-model tests**

Add these tests:

```csharp
[Fact]
public void AppComposition_uses_one_shared_session_log_and_coordinator()
{
    var composition = AppComposition.CreateForTesting(new EmptyNativeApi(), new FakeProcessRunner());

    Assert.Same(composition.Session, composition.MainViewModel.DeviceSession);
    Assert.Same(composition.LogService.Entries, composition.MainViewModel.Logs.Entries);
    Assert.Same(composition.Coordinator, composition.MainViewModel.Coordinator);
}

[Fact]
public async Task RefreshDeviceAsync_delegates_to_the_monitor_when_one_is_supplied()
{
    var session = new DeviceSessionViewModel();
    var monitor = new RecordingDeviceMonitor(session);
    var viewModel = new MainViewModel(session, deviceMonitor: monitor);

    await viewModel.RefreshDeviceAsync(logActivity: true);

    Assert.Equal(1, monitor.ManualRefreshCount);
}
```

Create `RecordingDeviceMonitor` as a test double behind a new `IDeviceMonitor` interface exposing `Task RefreshManualAsync(bool logActivity, CancellationToken cancellationToken = default)`, `Task StartAsync(CancellationToken)`, and `Task StopAsync()`.

- [ ] **Step 2: Run composition tests and verify failure**

Run: `dotnet test VivoKsu.slnx --filter "FullyQualifiedName~AppCompositionTests|FullyQualifiedName~MainViewModelTests"`

Expected: FAIL because `AppComposition`, `IDeviceMonitor`, and the new `MainViewModel` constructor argument do not exist.

- [ ] **Step 3: Add `IDeviceMonitor` and implement it on the monitor service**

Create `IDeviceMonitor.cs`:

```csharp
namespace VivoKsu.App.Services;

public interface IDeviceMonitor
{
    Task StartAsync(CancellationToken cancellationToken = default);
    Task StopAsync();
    Task RefreshManualAsync(bool logActivity, CancellationToken cancellationToken = default);
}
```

Make `DeviceMonitorService` implement this interface. Its existing `RefreshAutomaticallyAsync` remains public for the timer and tests but is intentionally absent from the smaller interface.

- [ ] **Step 4: Add `AppComposition` and explicit application startup**

Create `AppComposition` with public read-only properties `Session`, `LogService`, `Coordinator`, `Monitor`, and `MainViewModel`. Its `CreateDefault` method must build the dependency graph in this order:

```text
native API -> backend -> logs -> session -> coordinator -> device info -> session service
-> monitor -> quick flash service -> mirror service -> file service -> partition service
-> feature view models -> main view model
```

After creating `MainViewModel`, attach `Monitor.DeviceRefreshed += MainViewModel.OnDeviceRefreshedAsync`. Move the existing ROOT and line-flash quick-flash continuation wiring from `MainWindow` into `AppComposition`. Add `StartAsync` to start the monitor and `StopAsync` to stop the monitor then stop the mirror service.

Add `CreateForTesting(IFastbootRsNativeApi nativeApi, IProcessRunner processRunner)` using the supplied fake implementations instead of the default native API and process runner.

Change `App.xaml` by removing `StartupUri="MainWindow.xaml"`. In `App.xaml.cs`, override `OnStartup`, set a private composition field with `AppComposition.CreateDefault()`, create `new MainWindow(composition)`, set `MainWindow`, and call `Show()`. Override `OnExit` as `async void`, await `composition.StopAsync()`, then call `base.OnExit(e)`.

Change `MainWindow` to accept an `AppComposition` constructor argument, set `DataContext = composition.MainViewModel`, call `await composition.StartAsync()` from `Loaded`, and call `await composition.StopAsync()` from `Closed`. Delete `DispatcherTimer`, all direct service creation, and the private silent-refresh method.

- [ ] **Step 5: Delegate MainViewModel refresh and refresh-follow-up work**

Add optional `IDeviceMonitor? deviceMonitor = null` and `IOperationCoordinator? coordinator = null` constructor parameters to `MainViewModel`. Expose `public IOperationCoordinator? Coordinator => coordinator;` and keep the existing fallback path for tests that provide only `DeviceSessionService`.

When `deviceMonitor` is non-null, `RefreshDeviceAsync` must await `deviceMonitor.RefreshManualAsync(logActivity)` and return. Add this public method for the monitor event:

```csharp
public async Task OnDeviceRefreshedAsync(DeviceRefreshMode refreshMode, CancellationToken cancellationToken)
{
    if (DeviceSession.IsBusy)
    {
        return;
    }

    if (refreshMode == DeviceRefreshMode.Automatic)
    {
        await LineFlash.RefreshAutomaticallyAsync();
    }
    else
    {
        await LineFlash.RefreshAsync(logIfUnavailable: false);
    }

    if (!DeviceSession.IsBusy)
    {
        await Mirror.ReconcileAsync();
    }
}
```

Retain the same sequence in the no-monitor fallback path after `DeviceSessionService.RefreshAsync`.

- [ ] **Step 6: Run composition, main view model, and full tests**

Run: `dotnet test VivoKsu.slnx --filter "FullyQualifiedName~AppCompositionTests|FullyQualifiedName~MainViewModelTests"`

Expected: PASS.

Run: `dotnet test VivoKsu.slnx`

Expected: PASS with all existing tests.

- [ ] **Step 7: Commit when repository metadata exists**

```powershell
git add src/VivoKsu.App/App.xaml src/VivoKsu.App/App.xaml.cs src/VivoKsu.App/MainWindow.xaml.cs src/VivoKsu.App/Services/AppComposition.cs src/VivoKsu.App/Services/IDeviceMonitor.cs src/VivoKsu.App/Services/DeviceMonitorService.cs src/VivoKsu.App/ViewModels/MainViewModel.cs tests/VivoKsu.App.Tests/AppCompositionTests.cs tests/VivoKsu.App.Tests/MainViewModelTests.cs
git commit -m "refactor: compose VivoKsu services at application startup"
```

Run the commands only after `git rev-parse --is-inside-work-tree` succeeds.

## Task 4: Route overview reboot and quick flash through the coordinator

**Files:**
- Modify: `src/VivoKsu.App/ViewModels/OverviewViewModel.cs`
- Modify: `src/VivoKsu.App/ViewModels/QuickFlashViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/OverviewViewModelTests.cs`
- Test: `tests/VivoKsu.App.Tests/QuickFlashViewModelTests.cs`

**Interfaces:**
- Consumes: `IOperationCoordinator.RunAsync`, `CancelCurrent`, `OperationContext`, existing `FastbootRsBackend`, and existing `QuickFlashService`.
- Produces: reboot and flash task lifecycles with a common correlation ID, global cancel path, and no page-owned cancellation source for normal composed runtime.

- [ ] **Step 1: Add failing coordinator-integration tests**

Add this test to `OverviewViewModelTests.cs`:

```csharp
[Fact]
public async Task RebootBootloaderCommand_uses_the_shared_coordinator_lifecycle()
{
    var session = new DeviceSessionViewModel();
    session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "RF8", "ADB 已连接"));
    var logs = new OperationLogService();
    var coordinator = new OperationCoordinator(session, logs);
    var native = new RebootNativeApi();
    var viewModel = new OverviewViewModel(session, new FastbootRsBackend(native), logs, coordinator);

    await viewModel.RebootBootloaderCommand.ExecuteAsync(null);

    Assert.Equal(("RF8", "bootloader"), native.LastRebootRequest);
    Assert.False(coordinator.IsBusy);
    Assert.Contains(logs.Entries, entry => entry.OperationId is not null && entry.Level == OperationLogLevel.Success);
}
```

Add this test to `QuickFlashViewModelTests.cs`:

```csharp
[Fact]
public async Task CancelActiveFlashCommand_cancels_through_the_shared_coordinator()
{
    var native = new WaitingNativeApi();
    var logs = new OperationLogService();
    var session = new DeviceSessionViewModel();
    var coordinator = new OperationCoordinator(session, logs);
    var viewModel = new QuickFlashViewModel(session,
        new QuickFlashService(new FastbootRsBackend(native), logs), logs, coordinator)
    {
        SelectedImage = new FlashImageInfo("C:\\images\\boot.img", 1024)
    };

    var operation = viewModel.ConfirmFlashCommand.ExecuteAsync(null);
    await native.DiscoveryStarted.Task.WaitAsync(TimeSpan.FromSeconds(2));
    viewModel.CancelActiveFlashCommand.Execute(null);
    await operation;

    Assert.Equal(OperationKind.Canceled, session.OperationKind);
    Assert.False(coordinator.IsBusy);
}
```

- [ ] **Step 2: Run integration tests and verify failure**

Run: `dotnet test VivoKsu.slnx --filter "FullyQualifiedName~OverviewViewModelTests|FullyQualifiedName~QuickFlashViewModelTests"`

Expected: FAIL because the optional coordinator constructor parameter and coordinator-based cancellation do not exist.

- [ ] **Step 3: Migrate reboot without changing backend targets**

Add optional `IOperationCoordinator? coordinator = null` to `OverviewViewModel`. In the coordinator path, call `coordinator.RunAsync(OperationKind.Rebooting, status, async (context, token) => { context.ReportStage(status); await backend.RebootAsync(session.Serial, target, token); })`.

Catch `OperationCanceledException` and other exceptions inside the command method because the coordinator already writes the terminal session status and shared log. Retain the existing direct execution path when the optional coordinator is null so existing callers remain compatible. Keep native targets exactly `""`, `"bootloader"`, and `"fastboot"`.

- [ ] **Step 4: Migrate quick flash and cancellation**

Add optional `IOperationCoordinator? coordinator = null` to `QuickFlashViewModel`. Change the image picker filter to `Android image (*.img;*.bin)|*.img;*.bin` so it matches the existing service validation. In the coordinator path:

```csharp
await coordinator.RunAsync(
    OperationKind.Flashing,
    $"正在刷写 {SelectedPartition}",
    (_, token) => quickFlash.FlashAsync(session, SelectedPartition, SelectedImage, SelectedTarget, token));
```

Set `IsFlashOperationActive=true` before awaiting and reset it in `finally`. `CancelActiveFlash` must call `coordinator.CancelCurrent()` when a coordinator exists; otherwise preserve the existing page-owned `CancellationTokenSource` behavior. Keep the confirmation dialog and image-selection behavior unchanged.

Update `QuickFlashService` to accept an optional `OperationContext? context = null` on `FlashAsync` and `FlashRootImagesAsync`. Use `context?.ReportStage(...)` before waiting, before each partition flash, and before reboot. Keep existing direct `session.BeginOperation`/terminal behavior only when `context` is null. In the coordinator path, throw the original exception after stage reporting so the coordinator performs terminal-state/log handling exactly once.

- [ ] **Step 5: Run focused and full test suites**

Run: `dotnet test VivoKsu.slnx --filter "FullyQualifiedName~OverviewViewModelTests|FullyQualifiedName~QuickFlashViewModelTests|FullyQualifiedName~QuickFlashServiceTests"`

Expected: PASS.

Run: `dotnet test VivoKsu.slnx`

Expected: PASS with existing quick-flash cancellation and reboot behavior retained.

- [ ] **Step 6: Commit when repository metadata exists**

```powershell
git add src/VivoKsu.App/ViewModels/OverviewViewModel.cs src/VivoKsu.App/ViewModels/QuickFlashViewModel.cs src/VivoKsu.App/Services/QuickFlashService.cs tests/VivoKsu.App.Tests/OverviewViewModelTests.cs tests/VivoKsu.App.Tests/QuickFlashViewModelTests.cs
git commit -m "feat: route reboot and quick flash through operation coordinator"
```

Run the commands only after `git rev-parse --is-inside-work-tree` succeeds.

## Task 5: Verify framework delivery and document the next migration boundary

**Files:**
- Modify: `docs/superpowers/specs/2026-08-11-vivoksu-core-framework-design.md`
- Modify: `docs/superpowers/plans/2026-08-11-vivoksu-core-framework.md`
- Verify: `scripts/Publish-Release.ps1`

**Interfaces:**
- Consumes: all framework types and existing publish script.
- Produces: verified Release build and an explicit next increment for ROOT, file management, line flash, and mirror migration.

- [ ] **Step 1: Mark implemented delivery items in the design document**

Append an `## Implementation Note` section to the design document only after Tasks 1-4 pass:

```markdown
## Implementation Note

2026-08-11: OperationCoordinator, DeviceMonitorService, AppComposition, reboot migration, and quick-flash migration are implemented and covered by the framework test suite. ROOT, file management, line-flash extraction, and ADB mirror still use their existing task entry points and are the next migration increment.
```

- [ ] **Step 2: Run full framework verification**

Run: `dotnet test VivoKsu.slnx`

Expected: all tests pass.

Run: `dotnet build VivoKsu.slnx -c Release`

Expected: zero errors and zero warnings.

Run: `powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\Publish-Release.ps1`

Expected: a bundled x64 ZIP exists under `artifacts\release` and includes `scrcpy\scrcpy.exe`, `scrcpy\scrcpy-server`, `platform-tools\adb.exe`, and `root-tools\magiskboot.so`.

- [ ] **Step 3: Perform an application lifecycle smoke test**

Run: `Start-Process .\src\VivoKsu.App\bin\Release\net8.0-windows\VivoKsu.App.exe`

Expected: the window opens, automatic device refresh starts without a `DispatcherTimer` in `MainWindow`, the lower-left device state remains visible, and closing the window stops the monitor without a crash.

- [ ] **Step 4: Commit documentation when repository metadata exists**

```powershell
git add docs/superpowers/specs/2026-08-11-vivoksu-core-framework-design.md docs/superpowers/plans/2026-08-11-vivoksu-core-framework.md
git commit -m "docs: record core framework delivery"
```

Run the commands only after `git rev-parse --is-inside-work-tree` succeeds.

## Plan Self-Review

- Spec coverage: Task 1 implements serialized state, cancellation, correlated logging, and terminal state recovery. Task 2 implements automatic monitor gating, shared manual/automatic refresh gating, compensation refresh, and lifecycle management. Task 3 removes business composition and polling from `MainWindow`. Task 4 migrates reboot and quick flash without changing their native protocol behavior. Task 5 performs full test/build/publish and records the next migration boundary.
- Placeholder scan: every task contains concrete signatures, assertions, commands, and terminal expectations; each added type is defined before a later task consumes it.
- Type consistency: `OperationStateSnapshot`, `OperationContext`, `IOperationCoordinator`, `IDeviceRefreshService`, `IDeviceMonitor`, `DeviceMonitorService`, and `AppComposition` use the same names and signatures in all tasks. `OperationCoordinator.RunAsync` returns `Task` and preserves cancellation by rethrowing `OperationCanceledException` to the calling view model.
- Scope check: ROOT, file management, line flash, and mirror migration are intentionally deferred to the next execution plan because the core framework can be independently tested after Task 4.
