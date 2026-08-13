# VivoKsu ROOT Operation Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every ROOT foreground action through the shared operation coordinator while retaining the current Vivo KSU and official KernelSU algorithms.

**Architecture:** Extend `OperationContext` so a long operation can update both its stage and the current `OperationKind`. Pass an optional context through ROOT patching services, then make `RootViewModel` choose the shared coordinator path when it is supplied while keeping its existing direct path for constructor compatibility. `AppComposition` injects its unique coordinator into ROOT.

**Tech Stack:** .NET 8 WPF, C# 12, CommunityToolkit.Mvvm 8.4.0, xUnit, existing FastbootRs backend.

## Global Constraints

- Retain the fixed lower-left device status and fixed right-side log layout; this task changes no XAML.
- Keep `FastbootRsBackend` as the only managed/native ADB and Fastboot boundary.
- Preserve the existing Vivo KSU init_boot patching and official KernelSU vendor_boot processing commands.
- Reuse `QuickFlashService.FlashRootImagesAsync` for all ROOT flashing.
- One foreground device operation runs at a time; automatic discovery remains paused by the shared coordinator.
- Target Windows 10/11 x64 with bundled APK, `magiskboot.so`, platform-tools, and scrcpy resources.
- Do not introduce arbitrary partition flashing, a new UI library, Windows 7 support, or a visible global cancel control.

---

## File Structure

```text
src/VivoKsu.App/
├── Services/
│   ├── OperationContext.cs             # Stage and kind updates from nested workflows
│   ├── OperationCoordinator.cs         # Applies context kind changes to shared state
│   ├── VivoKsuDevicePatchService.cs    # Context-aware init_boot patch stages
│   ├── VivoVendorBootProcessor.cs      # Context-aware vendor_boot patch stages
│   └── AppComposition.cs               # Injects shared coordinator into ROOT
└── ViewModels/
    └── RootViewModel.cs                # Runs ROOT commands through coordinator

tests/VivoKsu.App.Tests/
├── OperationCoordinatorTests.cs        # Context kind transition regression
└── RootViewModelTests.cs                # Coordinated automatic ROOT lifecycle
```

## Task 1: Allow a context to change the active operation kind

**Files:**
- Modify: `src/VivoKsu.App/Services/OperationContext.cs`
- Modify: `src/VivoKsu.App/Services/OperationCoordinator.cs`
- Test: `tests/VivoKsu.App.Tests/OperationCoordinatorTests.cs`

**Interfaces:**
- Consumes: existing `OperationContext.ReportStage`, `OperationCoordinator.RunAsync`, and `DeviceSessionViewModel.BeginOperation`.
- Produces: `OperationContext.ReportStage(string stage, OperationKind? kind = null)` and coordinator state/session updates that preserve the current kind when `kind` is null.

- [ ] **Step 1: Write the failing context-kind test**

Append to `OperationCoordinatorTests.cs`:

```csharp
[Fact]
public async Task ReportStage_can_transition_the_active_operation_kind()
{
    var (coordinator, session, _) = CreateCoordinator();
    OperationStateSnapshot? reported = null;
    coordinator.StateChanged += (_, _) =>
    {
        if (coordinator.IsBusy && coordinator.State.Stage == "正在重启至 bootloader")
        {
            reported = coordinator.State;
        }
    };

    await coordinator.RunAsync(OperationKind.Installing, "ROOT 自动流程", (context, _) =>
    {
        context.ReportStage("正在重启至 bootloader", OperationKind.Rebooting);
        return Task.CompletedTask;
    });

    Assert.NotNull(reported);
    Assert.Equal(OperationKind.Rebooting, reported!.Kind);
    Assert.Equal(OperationKind.Rebooting, session.OperationKind);
}
```

- [ ] **Step 2: Run the focused test and verify the expected compiler failure**

Run:

```powershell
dotnet test VivoKsu.slnx --filter FullyQualifiedName~OperationCoordinatorTests
```

Expected: FAIL because `ReportStage` does not accept an `OperationKind` argument.

- [ ] **Step 3: Extend the operation context callback and public method**

Change `OperationContext` to store an `Action<string?, double?, OperationKind?>` callback. Preserve the existing one-argument call by adding the optional parameter:

```csharp
public void ReportStage(string stage, OperationKind? kind = null)
{
    ArgumentException.ThrowIfNullOrWhiteSpace(stage);
    report(stage, null, kind);
}

public void ReportProgress(double progress)
{
    if (progress is < 0 or > 1)
    {
        throw new ArgumentOutOfRangeException(nameof(progress));
    }

    report(null, progress, null);
}
```

Use the existing `ReportStage(string)` call sites unchanged.

- [ ] **Step 4: Update the coordinator report method without changing cancellation behavior**

Change the private report callback to accept `OperationKind? kind`. In the `state = current with` expression, set:

```csharp
Kind = kind ?? current.Kind,
Stage = stage ?? current.Stage,
Progress = progress ?? current.Progress
```

After releasing `stateGate`, continue to call `session.BeginOperation(current.Kind, current.Stage)`, write the same correlated stage log, and raise `StateChanged`. Construct the context with the updated callback.

- [ ] **Step 5: Run coordinator and full regression tests**

Run:

```powershell
dotnet test VivoKsu.slnx --filter FullyQualifiedName~OperationCoordinatorTests
dotnet test VivoKsu.slnx
```

Expected: the new context-kind test passes and the existing coordinator/monitor workflows retain their behavior.

- [ ] **Step 6: Commit when repository metadata exists**

Run `git rev-parse --is-inside-work-tree`. Only when it returns `true`:

```powershell
git add src/VivoKsu.App/Services/OperationContext.cs src/VivoKsu.App/Services/OperationCoordinator.cs tests/VivoKsu.App.Tests/OperationCoordinatorTests.cs
git commit -m "feat: allow staged operation kind transitions"
```

## Task 2: Make ROOT patch services context-aware

**Files:**
- Modify: `src/VivoKsu.App/Services/VivoKsuDevicePatchService.cs`
- Modify: `src/VivoKsu.App/Services/VivoVendorBootProcessor.cs`
- Test: `tests/VivoKsu.App.Tests/VivoKsuDevicePatchServiceTests.cs`
- Test: `tests/VivoKsu.App.Tests/VivoVendorBootProcessorTests.cs`

**Interfaces:**
- Consumes: `OperationContext`, existing patch inputs, FastbootRs backend, and current cleanup semantics.
- Produces: optional trailing `OperationContext? context = null` parameters on both `PatchAsync` methods; context-backed stages before local resource setup, ADB upload, remote patching, and pulling the output.

- [ ] **Step 1: Add failing context-stage assertions to both patch-service tests**

Wrap each existing successful service scenario in a real `OperationCoordinator.RunAsync` call. Pass the delegate's internal `OperationContext` as the final `PatchAsync` argument, capture the operation ID from the coordinator's first Info log, and assert that entries with that same ID contain these boundaries:

```csharp
var operationId = Assert.Single(logs.Entries.Where(entry => entry.Message == "正在修补 ROOT 镜像")).OperationId;
Assert.NotNull(operationId);
Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在准备 ROOT 修补资源", StringComparison.Ordinal));
Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在上传 init_boot", StringComparison.Ordinal));
Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在获取修补后的 init_boot", StringComparison.Ordinal));
```

For vendor_boot, use the same wrapper and assert `正在上传 vendor_boot`, `正在处理 vendor_boot`, and `正在获取修补后的 vendor_boot`. This keeps `OperationContext` construction inside the coordinator boundary.

- [ ] **Step 2: Run patch-service tests and verify failure**

Run:

```powershell
dotnet test VivoKsu.slnx --filter "FullyQualifiedName~VivoKsuDevicePatchServiceTests|FullyQualifiedName~VivoVendorBootProcessorTests"
```

Expected: FAIL because the patch-service methods do not accept the context argument.

- [ ] **Step 3: Add init_boot patch stages without changing its algorithm**

Change `VivoKsuDevicePatchService.PatchAsync` to:

```csharp
public async Task<FlashImageInfo> PatchAsync(
    string serial,
    string managerKey,
    string kmi,
    FlashImageInfo source,
    CancellationToken cancellationToken,
    OperationContext? context = null)
```

Call `context?.ReportStage(...)` before `ExtractVerifiedLibKsud`, before the two `PushAsync` calls, before the `boot-patch` shell command, and before `PullAsync`. Use the exact labels asserted above; do not add standalone terminal logs. Keep the `finally` cleanup calls and their `CancellationToken.None` best-effort behavior unchanged.

- [ ] **Step 4: Add vendor_boot patch stages without changing shell commands**

Add the same optional trailing context parameter to `VivoVendorBootProcessor.PatchAsync`. Report the labels asserted in Step 1 before pushing `magiskboot`/`vendor_boot`, before unpack/filter/repack shell processing, and before pulling `new-boot.img`. Keep `ResolveTargetDirectory`, GKI selection, file validation, and best-effort remote cleanup unchanged.

- [ ] **Step 5: Run focused patch tests and full regression**

Run:

```powershell
dotnet test VivoKsu.slnx --filter "FullyQualifiedName~VivoKsuDevicePatchServiceTests|FullyQualifiedName~VivoVendorBootProcessorTests"
dotnet test VivoKsu.slnx
```

Expected: ROOT image outputs, validation, vendor filtering, and cleanup tests continue to pass while stage assertions become green.

- [ ] **Step 6: Commit when repository metadata exists**

Only in a Git worktree:

```powershell
git add src/VivoKsu.App/Services/VivoKsuDevicePatchService.cs src/VivoKsu.App/Services/VivoVendorBootProcessor.cs tests/VivoKsu.App.Tests/VivoKsuDevicePatchServiceTests.cs tests/VivoKsu.App.Tests/VivoVendorBootProcessorTests.cs
git commit -m "feat: report ROOT patch stages through operation context"
```

## Task 3: Route ROOT commands through the shared coordinator

**Files:**
- Modify: `src/VivoKsu.App/ViewModels/RootViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/RootViewModelTests.cs`

**Interfaces:**
- Consumes: `IOperationCoordinator`, context-aware ROOT patch services, `QuickFlashService.FlashRootImagesAsync`, and existing direct ROOT command behavior.
- Produces: an optional `IOperationCoordinator? coordinator = null` constructor parameter; coordinated install, KMI, manual patch, and automatic root command paths.

- [ ] **Step 1: Write the failing coordinated automatic-flow test**

Add a test which creates a temporary `init_boot.img`, an ADB-connected session with kernel `6.1.75`, `OperationLogService`, `OperationCoordinator`, and a fake backend whose `Reboot(..., "bootloader")` changes `ListDevices()` from `ADB123\tdevice` to `FAST123\tfastboot`.

The fake must return `package:/data/app/manager.apk` for `pm path`, create the requested local file and return its length for the init_boot patched-image `Pull`, and record `Flash` calls. Construct ROOT with the coordinator, set `SelectedImage`, execute `RunAutomaticRootCommand`, then assert:

```csharp
Assert.Contains(("ADB123", "bootloader"), native.Reboots);
Assert.Contains(("FAST123", "init_boot", imagePath), native.Flashes);
Assert.False(coordinator.IsBusy);
Assert.Equal(OperationKind.Completed, session.OperationKind);
Assert.Contains(logs.Entries, entry => entry.OperationId is not null && entry.Level == OperationLogLevel.Success);
```

Delete the temporary directory in `finally`.

- [ ] **Step 2: Run ROOT view-model tests and verify failure**

Run:

```powershell
dotnet test VivoKsu.slnx --filter FullyQualifiedName~RootViewModelTests
```

Expected: FAIL because `RootViewModel` has no coordinator constructor parameter and its automatic path does not create a correlated operation.

- [ ] **Step 3: Add the optional coordinator and a single coordinator helper**

Add `private readonly IOperationCoordinator? coordinator;` and an optional final constructor parameter. Preserve both existing constructor overloads by forwarding `null` when their callers do not supply a coordinator.

Add a private helper that runs a supplied delegate directly when `coordinator` is null, otherwise awaits `coordinator.RunAsync`. In the coordinator branch catch `OperationCanceledException` and general exceptions at the command boundary only; do not call `session.CancelOperation`, `FailOperation`, or duplicate terminal log entries because `OperationCoordinator` already does that.

- [ ] **Step 4: Migrate image inspection, manager installation, KMI, and manual patching**

For every existing command that calls `session.BeginOperation`, retain the direct branch exactly and add a coordinator branch:

```csharp
await coordinator.RunAsync(OperationKind.Hashing, "正在修补 ROOT 镜像", async (context, token) =>
{
    context.ReportStage($"正在修补 {SelectedManagerLabel} 镜像");
    await PatchImagesCoreAsync(token, context);
    context.ReportStage("ROOT 镜像修补完成");
});
```

Use `Hashing` for image inspection/patching, `Installing` for manager install, and `Discovering` for `uname -r` KMI resolution. Change `InstallManagerCoreAsync` and `PatchImagesCoreAsync` to accept optional context, report their command boundaries, and pass context into the two patch services. Keep the image-selection dialogs, manager verification, device checks, and continuation methods unchanged.

- [ ] **Step 5: Make the automatic ROOT flow one staged coordinator operation**

In the coordinator path, run one task initially marked `Installing` with title `ROOT 自动流程`. It must report and change kinds in this order:

```csharp
context.ReportStage($"ROOT 自动流程: 正在安装 {SelectedManagerLabel}", OperationKind.Installing);
await InstallManagerCoreAsync(token, context);
context.ReportStage("ROOT 自动流程: 正在修补镜像", OperationKind.Hashing);
await PatchImagesCoreAsync(token, context);
context.ReportStage("ROOT 自动流程: 正在重启至 bootloader", OperationKind.Rebooting);
await backend.RebootAsync(session.Serial, "bootloader", token);
context.ReportStage("ROOT 自动流程: 正在等待并刷写 ROOT 镜像", OperationKind.Flashing);
await imageInspector.FlashRootImagesAsync(session, images, FastbootTarget.Fastboot, token, context);
```

Build `images` exactly as the current code does: init_boot always, vendor_boot only for official KernelSU with a generated vendor image. Preserve the current direct path for callers without a coordinator.

- [ ] **Step 6: Run ROOT-focused and full regression tests**

Run:

```powershell
dotnet test VivoKsu.slnx --filter "FullyQualifiedName~RootViewModelTests|FullyQualifiedName~QuickFlashServiceTests|FullyQualifiedName~OperationCoordinatorTests"
dotnet test VivoKsu.slnx
```

Expected: the automatic flow keeps one correlated lifecycle through ADB patching and fastboot flash; direct ROOT tests remain compatible.

- [ ] **Step 7: Commit when repository metadata exists**

Only in a Git worktree:

```powershell
git add src/VivoKsu.App/ViewModels/RootViewModel.cs tests/VivoKsu.App.Tests/RootViewModelTests.cs
git commit -m "feat: coordinate ROOT workflows"
```

## Task 4: Inject ROOT's shared coordinator and verify the release path

**Files:**
- Modify: `src/VivoKsu.App/Services/AppComposition.cs`
- Modify: `docs/superpowers/specs/2026-08-11-vivoksu-feature-operation-migration-design.md`
- Test: `tests/VivoKsu.App.Tests/AppCompositionTests.cs`
- Verify: `scripts/Publish-Release.ps1`

**Interfaces:**
- Consumes: composed `Coordinator`, coordinator-aware `RootViewModel`, and the existing application lifecycle.
- Produces: a single ROOT/coordinator instance in production composition plus an implementation note with verified checks.

- [ ] **Step 1: Add a failing composition assertion**

Add a test to `AppCompositionTests.cs` that obtains `composition.MainViewModel.Root` and asserts its public coordinator exposure references `composition.Coordinator`:

```csharp
Assert.Same(composition.Coordinator, composition.MainViewModel.Root.Coordinator);
```

Expose `public IOperationCoordinator? Coordinator => coordinator;` from `RootViewModel` solely for composition/test visibility, matching `MainViewModel`'s existing pattern.

- [ ] **Step 2: Run composition tests and verify failure**

Run:

```powershell
dotnet test VivoKsu.slnx --filter FullyQualifiedName~AppCompositionTests
```

Expected: FAIL because composition does not yet pass the coordinator to ROOT.

- [ ] **Step 3: Wire the composition root**

Pass `Coordinator` as the final `RootViewModel` constructor argument in `AppComposition`. Do not create another coordinator or alter monitor/mirror shutdown ordering.

- [ ] **Step 4: Run final validation and document the implementation**

Run:

```powershell
dotnet test VivoKsu.slnx
dotnet build VivoKsu.slnx -c Release
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\Publish-Release.ps1
```

After all three commands pass, append this paragraph to the feature-migration design:

```markdown
## ROOT Implementation Note

2026-08-11: ROOT image inspection, manager installation, KMI resolution, manual patching, and the automatic ROOT flow now use the shared operation coordinator. The automatic flow reports install, patch, reboot, and flash stages through a single correlated operation. File management, line flash, and mirror migration remain the next increments.
```

- [ ] **Step 5: Inspect the published archive**

Use `System.IO.Compression.ZipFile` to assert `VivoKsu-win-x64.zip` contains:

```text
scrcpy/scrcpy.exe
scrcpy/scrcpy-server
platform-tools/adb.exe
root-tools/magiskboot.so
apk/KernelSU.apk
apk/KSU.APK
```

Also verify no archive entry ends in `libksud.exe` or `ksud.exe`, and verify every `SHA256SUMS.txt` entry matches its file in `artifacts\release\VivoKsu-win-x64`.

- [ ] **Step 6: Commit when repository metadata exists**

Only in a Git worktree:

```powershell
git add src/VivoKsu.App/Services/AppComposition.cs docs/superpowers/specs/2026-08-11-vivoksu-feature-operation-migration-design.md tests/VivoKsu.App.Tests/AppCompositionTests.cs
git commit -m "refactor: compose ROOT operations through coordinator"
```

## Plan Self-Review

- Spec coverage: Task 1 implements staged kind changes; Task 2 provides correlated ROOT patch boundaries; Task 3 migrates all ROOT command paths, including the one-task automatic chain; Task 4 supplies the shared production dependency and verifies both package and documentation.
- Placeholder scan: all tests, signatures, labels, commands, and archive entries are concrete; no `TBD`, `TODO`, or deferred code instructions remain.
- Type consistency: `ReportStage(string, OperationKind? = null)` is created before ROOT uses it; both patch services use the same trailing `OperationContext? context = null`; `RootViewModel` receives the optional `IOperationCoordinator? coordinator = null` before composition passes it.
- Scope check: file management, line flash, and mirror are explicitly deferred to the next independently testable plans, preserving the migration order in the design.
