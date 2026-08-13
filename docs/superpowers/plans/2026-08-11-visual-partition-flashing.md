# Visual Partition Flashing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the line-flash placeholder with a visual workspace that reads all available partitions and backs up, writes, or erases selected partitions through ADB Root or Fastboot.

**Architecture:** `PartitionWorkspaceViewModel` owns the dense table and immutable execution requests. `FastbootPartitionTransport` uses the existing fastboot-rs boundary, while `AdbRootPartitionTransport` uses the bundled `adb.exe` for Root discovery and streamed block-device transfers. One `PartitionExecutionService` serializes all tasks through `OperationCoordinator`.

**Tech Stack:** .NET 8 WPF, CommunityToolkit.Mvvm, HandyControl, bundled Android platform-tools, fastboot-rs Rust DLL, xUnit and FluentAssertions.

## Global Constraints

- Keep the existing white/teal WPF shell, fixed right-side log, and lower-left device status.
- Support only manual/folder mapping of `.img` and `.bin`; do not enforce image filenames.
- Do not parse BAT/XML or support EDL/Firehose.
- Every discovered partition is operable after confirmation; risk labels are informational only.
- Keep backup, write, and erase as separate sequential queues. Stop after the first failed task; do not retry or roll back writes.
- Keep automatic device polling paused while an operation is owned by `OperationCoordinator`.
- Do not add SHA-256 validation, auto reboot, or automatic slot switching.
- The workspace is not a Git repository; omit commit steps.

---

## File Structure

| Path | Responsibility |
| --- | --- |
| `src/VivoKsu.App/Models/PartitionModels.cs` | Transport, operation, snapshot, task, error, and progress records. |
| `src/VivoKsu.App/Services/IPartitionTransport.cs` | Shared discovery/backup/write/erase contract. |
| `src/VivoKsu.App/Services/FastbootPartitionTransport.cs` | `getvar all` parser plus Fastboot execution. |
| `src/VivoKsu.App/Services/AdbRootPartitionTransport.cs` | Root `by-name` discovery and streamed `dd`/erase operations. |
| `src/VivoKsu.App/Services/PartitionExecutionService.cs` | Immutable plan creation and sequential execution. |
| `src/VivoKsu.App/ViewModels/PartitionRowViewModel.cs` | One selectable partition-table row. |
| `src/VivoKsu.App/ViewModels/PartitionWorkspaceViewModel.cs` | Page commands, filtering, mapping, and progress. |
| `src/VivoKsu.App/MainWindow.xaml` | Approved visual-flash layout. |
| Existing native API, composition, and tests | Fastboot erase/fetch bindings, dependency injection, and regressions. |

### Task 1: Add Shared Partition Models And Plan Builder

**Files:**

- Create: `src/VivoKsu.App/Models/PartitionModels.cs`
- Create: `src/VivoKsu.App/Services/IPartitionTransport.cs`
- Create: `src/VivoKsu.App/Services/PartitionExecutionPlanBuilder.cs`
- Create: `tests/VivoKsu.App.Tests/PartitionExecutionPlanBuilderTests.cs`

**Interfaces:**

```csharp
public enum PartitionTransportKind { Automatic, AdbRoot, Fastboot }
public enum PartitionOperationKind { Backup, Write, Erase }
public enum PartitionTaskState { Waiting, Running, Succeeded, Failed, Canceled }

public sealed record DevicePartition(
    string Name, string DevicePath, long? SizeBytes, string Slot,
    bool IsMounted, bool IsHighRisk, bool CanBackup);

public sealed record PartitionSnapshot(
    string Serial, PartitionTransportKind Transport, string ActiveSlot,
    IReadOnlyList<DevicePartition> Partitions);

public sealed record PartitionTask(
    string PartitionName, string DevicePath, string? ImagePath, string? OutputPath);

public sealed record PartitionExecutionPlan(
    string Serial, PartitionTransportKind Transport, PartitionOperationKind Operation,
    IReadOnlyList<PartitionTask> Tasks);

public sealed record PartitionTransferProgress(
    string PartitionName, long TransferredBytes, long? TotalBytes, double BytesPerSecond);

public sealed class PartitionOperationException : Exception
{
    public PartitionOperationException(PartitionTransportKind transport, string partitionName, string stage, Exception innerException);
}
```

- [ ] **Step 1: Write failing plan-builder tests.**

```csharp
[Fact]
public void Build_write_plan_keeps_the_selected_partition_and_any_img_filename()
{
    var plan = new PartitionExecutionPlanBuilder().BuildWrite(
        "FAST123", PartitionTransportKind.Fastboot,
        [new DevicePartition("boot_a", "boot_a", 64L * 1024 * 1024, "a", false, false, false)],
        new Dictionary<string, string> { ["boot_a"] = @"D:\\images\\custom.bin" });

    plan.Tasks.Should().ContainSingle();
    plan.Tasks[0].ImagePath.Should().Be(@"D:\\images\\custom.bin");
}
```

- [ ] **Step 2: Run the focused test and confirm it fails because the plan types do not exist.**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter FullyQualifiedName~PartitionExecutionPlanBuilderTests`

- [ ] **Step 3: Add immutable models and a builder.** The builder must copy selected row values into new `PartitionTask` records, reject mixed operation kinds, and leave high-risk/mounted items in the plan.

- [ ] **Step 4: Add tests for backup output paths, erase tasks without image paths, and immutable plans after the source mapping changes.**

- [ ] **Step 5: Re-run the focused test project and require all new tests to pass.**

### Task 2: Implement Fastboot Discovery, Erase, And Fetch

**Files:**

- Create: `src/VivoKsu.App/Services/FastbootPartitionTransport.cs`
- Modify: `src/VivoKsu.App/Services/FastbootRsBackend.cs`
- Modify: `src/VivoKsu.App/Services/IFastbootRsNativeApi.cs`
- Modify: `src/VivoKsu.App/Services/FastbootRsNative.cs`
- Modify: `src/VivoKsu.App/Services/NativeFastbootRsApi.cs`
- Modify: `src/VivoKsu.App/Services/FastbootRsApiFactory.cs`
- Modify: `src/VivoKsu.App/Services/PlatformToolsNativeApi.cs`
- Test: `tests/VivoKsu.App.Tests/FastbootPartitionTransportTests.cs`
- Test: `tests/VivoKsu.App.Tests/FastbootRsBackendTests.cs`

**Interfaces:**

```csharp
public interface IPartitionTransport
{
    PartitionTransportKind Kind { get; }
    Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken);
    Task BackupAsync(PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken);
    Task WriteAsync(PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken);
    Task EraseAsync(PartitionTask task, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken);
}
```

- [ ] **Step 1: Add failing parser tests for the real `getvar all` fields.**

```csharp
[Fact]
public async Task DiscoverAsync_returns_every_partition_size_reported_by_getvar_all()
{
    var api = new FakeNativeApi {
        GetVarAll = "partition-size:boot_a:0x04000000\npartition-size:super:0x200000000\ncurrent-slot:a"
    };
    var snapshot = await new FastbootPartitionTransport(new FastbootRsBackend(api))
        .DiscoverAsync("FAST123", CancellationToken.None);

    snapshot.Partitions.Select(x => x.Name).Should().Contain(["boot_a", "super"]);
    snapshot.ActiveSlot.Should().Be("a");
}
```

- [ ] **Step 2: Expose native Fastboot erase and fetch through `IFastbootRsNativeApi` and `FastbootRsBackend`.** Bind existing Rust exports `fastboot_erase` and `fastboot_fetch`; platform-tools fallback runs `fastboot erase <partition>` and `fastboot fetch <partition> <path>`.

- [ ] **Step 3: Implement `FastbootPartitionTransport`.** Parse all `partition-size:*` rows, normalize slots, create high-risk labels from partition names, flash through `FlashAsync`, erase through `EraseAsync`, and backup through `FetchAsync`. A fetch failure must return a typed `PartitionOperationException` and leave the output cleanup to the execution service.

- [ ] **Step 4: Add focused tests for erase/fetch forwarding and unsupported fetch failure.**

- [ ] **Step 5: Run Fastboot transport and existing native/backend regressions.**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~FastbootPartitionTransportTests|FullyQualifiedName~FastbootRsBackendTests"`

### Task 3: Implement ADB Root Partition Discovery And Streaming

**Files:**

- Create: `src/VivoKsu.App/Services/AdbRootPartitionTransport.cs`
- Create: `src/VivoKsu.App/Services/AdbRootTransferRunner.cs`
- Test: `tests/VivoKsu.App.Tests/AdbRootPartitionTransportTests.cs`

**Interfaces:**

```csharp
public interface IAdbRootTransferRunner
{
    Task<string> RunRootAsync(string serial, string command, CancellationToken cancellationToken);
    Task CopyFromDeviceAsync(string serial, string devicePath, string localPartialPath,
        IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken);
    Task CopyToDeviceAsync(string serial, string localImagePath, string devicePath,
        IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken);
}
```

- [ ] **Step 1: Write discovery tests using deterministic Root command output.** Cover duplicate `by-name` links, slot suffixes, capacity, and mounted partition flags.

```csharp
[Fact]
public async Task DiscoverAsync_deduplicates_by_name_links_and_keeps_mounted_partitions()
{
    var runner = new FakeAdbRootRunner("boot_a|/dev/block/sda12|67108864|0\nboot_a|/dev/block/sda12|67108864|0\nsuper|/dev/block/sda70|8589934592|1");
    var snapshot = await new AdbRootPartitionTransport(runner).DiscoverAsync("ADB123", CancellationToken.None);

    snapshot.Partitions.Should().HaveCount(2);
    snapshot.Partitions.Single(x => x.Name == "super").IsMounted.Should().BeTrue();
}
```

- [ ] **Step 2: Implement discovery through `adb shell su -c`.** Probe UID 0, enumerate existing `by-name` directories in one static shell script, resolve every link, get byte size with `blockdev --getsize64`, and mark mounts from `/proc/mounts`. Never incorporate a user-controlled partition name into this discovery command.

- [ ] **Step 3: Implement streamed transfers with bundled `adb.exe`.** Backup runs `adb exec-out ... su -c dd if=<resolved-path>` into `<output>.partial`; write streams the local file to `adb shell -T ... su -c dd of=<resolved-path> bs=4M conv=fsync`; erase runs `blkdiscard` and falls back to zero-fill when it exits non-zero. Report transferred bytes from the local stream loop.

- [ ] **Step 4: Add tests for Root loss, `.partial` cleanup delegation, output progress, write direction, and erase fallback command construction.**

- [ ] **Step 5: Run the ADB Root transport test class.**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter FullyQualifiedName~AdbRootPartitionTransportTests`

### Task 4: Execute Immutable Queues Through The Coordinator

**Files:**

- Create: `src/VivoKsu.App/Services/PartitionExecutionService.cs`
- Test: `tests/VivoKsu.App.Tests/PartitionExecutionServiceTests.cs`
- Modify: `src/VivoKsu.App/Models/OperationKind.cs`

**Interfaces:**

```csharp
public Task ExecuteAsync(
    PartitionExecutionPlan plan,
    Action<string, PartitionTaskState> setRowState,
    Action<PartitionTransferProgress> setProgress,
    CancellationToken cancellationToken);
```

- [ ] **Step 1: Write a failing execution-order test.**

```csharp
[Fact]
public async Task ExecuteAsync_stops_after_the_first_failed_partition()
{
    var transport = new RecordingPartitionTransport(failOn: "init_boot_a");
    var service = new PartitionExecutionService(transport, coordinator, logs);

    await Assert.ThrowsAsync<PartitionOperationException>(() => service.ExecuteAsync(plan, SetRowState, SetProgress, CancellationToken.None));

    transport.Writes.Should().Equal("boot_a", "init_boot_a");
}
```

- [ ] **Step 2: Run the test and confirm the service does not exist.**

- [ ] **Step 3: Implement coordinator-owned sequential execution.** Verify the connected serial and selected transport before each task, set `Waiting → Running → Succeeded/Failed/Canceled`, remove incomplete backup files after failed/canceled backup, and allow cancellation only between native task boundaries.

- [ ] **Step 4: Add tests for cancellation, serial mismatch, selected high-risk partitions, and progress aggregation.**

- [ ] **Step 5: Run execution and existing coordinator regressions.**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~PartitionExecutionServiceTests|FullyQualifiedName~OperationCoordinatorTests"`

### Task 5: Replace The Placeholder With The Approved WPF Workspace

**Files:**

- Create: `src/VivoKsu.App/ViewModels/PartitionRowViewModel.cs`
- Create: `src/VivoKsu.App/ViewModels/PartitionWorkspaceViewModel.cs`
- Modify: `src/VivoKsu.App/ViewModels/MainViewModel.cs`
- Modify: `src/VivoKsu.App/Services/AppComposition.cs`
- Modify: `src/VivoKsu.App/MainWindow.xaml`
- Modify: `src/VivoKsu.App/Models/AppPage.cs`
- Remove after migration: `src/VivoKsu.App/ViewModels/LineFlashViewModel.cs`, `src/VivoKsu.App/Services/FastbootPartitionService.cs`
- Test: `tests/VivoKsu.App.Tests/PartitionWorkspaceViewModelTests.cs`
- Remove after migration: `tests/VivoKsu.App.Tests/LineFlashViewModelTests.cs`, `tests/VivoKsu.App.Tests/FastbootPartitionServiceTests.cs`

**Interfaces:**

```csharp
public partial class PartitionWorkspaceViewModel : ObservableObject
{
    public ObservableCollection<PartitionRowViewModel> Partitions { get; }
    public PartitionTransportKind SelectedTransport { get; set; }
    public string FilterText { get; set; }
    public double OverallProgress { get; }
    public IAsyncRelayCommand RefreshCommand { get; }
    public IAsyncRelayCommand BackupSelectedCommand { get; }
    public IAsyncRelayCommand WriteSelectedCommand { get; }
    public IAsyncRelayCommand EraseSelectedCommand { get; }
    public IRelayCommand StopCommand { get; }
}
```

- [ ] **Step 1: Write ViewModel tests for transport selection, exact folder mapping, slotless active-slot mapping, and all-risk rows being executable.**

```csharp
[Fact]
public void MapImages_assigns_boot_img_to_the_active_slot_without_rejecting_the_filename()
{
    viewModel.ActiveSlot = "b";
    viewModel.MapImages([new FlashImageInfo(@"D:\\fw\\boot.img", 1024)]);

    viewModel.Partitions.Single(x => x.Name == "boot_b").ImagePath.Should().Be(@"D:\\fw\\boot.img");
}
```

- [ ] **Step 2: Implement compact row and workspace state.** Keep automatic refresh non-destructive: while no compatible device is present, retain the last snapshot rather than replacing it with fake rows. Re-read a complete snapshot when the selected transport is ready.

- [ ] **Step 3: Replace the `AppPage.LineFlash` content.** Rename the navigation label and page heading to `可视刷写`; use the approved layout: top segmented transport selector, summary strip, dense searchable partition table, bottom operations/progress, fixed right log. Remove the old ZIP inspection and controlled three-partition panels from this page.

- [ ] **Step 4: Wire composition and page refresh.** Construct both transports and the execution service in `AppComposition`; expose `PartitionWorkspace` from `MainViewModel`; call its non-destructive automatic refresh from `OnDeviceRefreshedAsync`.

- [ ] **Step 5: Run ViewModel regressions, then launch the app and inspect `1320x720` and `1460x780` screenshots.**

Run: `dotnet test tests/VivoKsu.App.Tests/VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~PartitionWorkspaceViewModelTests|FullyQualifiedName~MainViewModelTests"`

### Task 6: Full Verification And Publish

**Files:**

- Modify when needed: `docs/superpowers/specs/2026-08-11-visual-partition-flashing-design.md`
- Create: `artifacts/verification/visual-partition-flash-1320x720.png`
- Create: `artifacts/verification/visual-partition-flash-1460x780.png`

- [ ] **Step 1: Run all unit tests.**

Run: `dotnet test VivoKsu.slnx`

- [ ] **Step 2: Run a Release build with warnings treated as failures.**

Run: `dotnet build VivoKsu.slnx -c Release -warnaserror`

- [ ] **Step 3: Publish the bundled desktop build.**

Run: `dotnet publish src/VivoKsu.App/VivoKsu.App.csproj -c Release -r win-x64 --self-contained true -o artifacts/release/VivoKsu-win-x64`

- [ ] **Step 4: Perform controlled device smoke checks.** Read ADB Root and Fastboot tables, back up a small partition, verify queue stop after a simulated failure, then check the final log and lower-left device state. Do not include real partition erase/write in automated tests.

- [ ] **Step 5: Update the design document only if verification reveals a confirmed behavior change, then record the exact test/build/publish results in the final handoff.**

## Plan Self-Review

- Spec coverage: Tasks 1-5 cover every confirmed UI, dual-transport, execution, warning-only, and progress requirement; Task 6 covers verification and packaging.
- Placeholder scan: no unfinished implementation markers or implicit error-handling tasks are present.
- Type consistency: `PartitionTransportKind`, `PartitionOperationKind`, `PartitionSnapshot`, `PartitionTask`, `PartitionExecutionPlan`, `PartitionTransferProgress`, and `IPartitionTransport` are defined before their later consumers.
