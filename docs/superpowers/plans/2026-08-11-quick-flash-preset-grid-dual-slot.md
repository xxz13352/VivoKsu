# Quick Flash Preset Grid And Dual-Slot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace single-image quick flash with an eight-partition preset grid, batch and per-row flashing, A/B dual-slot writes, opposite-slot activation, optional waiting, and optional reboot.

**Architecture:** Keep the controlled partition enum as the whitelist, represent UI rows with `QuickFlashPresetItemViewModel`, and snapshot selections into immutable `QuickFlashExecutionPlan` values before confirmation. Generalize `QuickFlashService` into one ordered multi-image execution path and expose the already-bundled Rust `fastboot_set_active` export through the existing C# backend boundary.

**Tech Stack:** .NET 8, WPF, CommunityToolkit.Mvvm 8.4, HandyControl 3.5.1, xUnit, bundled Rust `fastboot-rs` DLL, bundled Android platform-tools fallback.

## Global Constraints

- Preserve the existing light white/teal/signal theme, navigation, lower-left device state, and fixed right-side log.
- Keep the quick-flash whitelist limited to `system`, `system_ext`, `product`, `odm`, `vendor`, `vendor_boot`, `boot`, and `init_boot`.
- Accept `.img` and `.bin` image filenames without enforcing partition-name matching.
- Dual-slot mode writes the same image to `<partition>_a` and `<partition>_b`, in that order, for every selected partition.
- Slot switching reads the original `current-slot` before any write and activates the opposite slot only after every write succeeds.
- `刷完切槽` is available only while `双刷入双槽` is enabled.
- Do not add arbitrary partition input, firmware auto-discovery, or separate A/B image selection.
- All foreground execution must use the shared `OperationCoordinator` and correlated right-side log.
- The workspace is not a Git repository. Do not initialize Git and replace commit steps with explicit review checkpoints.
- Follow strict TDD: add one behavioral test, observe the expected failure, add minimal production code, and observe it pass before continuing.

### User Revision

The final quick-flash whitelist is limited to `boot`, `init_boot`, `vendor_boot`, and `lk`. The original eight-partition draft below is superseded for implementation and verification.

---

## File Structure

**Create:**

- `src/VivoKsu.App/Models/QuickFlashRequest.cs`: immutable flash request, options, and confirmed execution-plan records.
- `src/VivoKsu.App/ViewModels/QuickFlashPresetItemViewModel.cs`: one observable preset row and selected image.

**Modify:**

- `src/VivoKsu.App/Models/QuickFlashPartition.cs`: complete eight-value whitelist.
- `src/VivoKsu.App/Services/IFastbootRsNativeApi.cs`: `SetActive` backend capability.
- `src/VivoKsu.App/Services/FastbootRsNative.cs`: `fastboot_set_active` P/Invoke.
- `src/VivoKsu.App/Services/NativeFastbootRsApi.cs`: native slot activation.
- `src/VivoKsu.App/Services/PlatformToolsNativeApi.cs`: platform-tools slot activation fallback.
- `src/VivoKsu.App/Services/FastbootRsApiFactory.cs`: composite slot activation forwarding.
- `src/VivoKsu.App/Services/FastbootRsBackend.cs`: async slot activation method.
- `src/VivoKsu.App/Services/QuickFlashService.cs`: preflight, multi-image queue, dual-slot expansion, switching, optional reboot, and optional waiting.
- `src/VivoKsu.App/ViewModels/QuickFlashViewModel.cs`: preset collection, option state, parameterized commands, immutable confirmation snapshot, and ROOT handoff compatibility.
- `src/VivoKsu.App/App.xaml`: compact flash option and image-path styles only when reusable style extraction reduces XAML duplication.
- `src/VivoKsu.App/MainWindow.xaml`: compact top toolbar, two-column preset grid, and confirmation summary.
- `docs/superpowers/specs/2026-08-11-quick-flash-preset-grid-dual-slot-design.md`: append implementation status after verification.

**Test:**

- `tests/VivoKsu.App.Tests/PlatformToolsNativeApiTests.cs`
- `tests/VivoKsu.App.Tests/FastbootRsBackendTests.cs`
- `tests/VivoKsu.App.Tests/QuickFlashServiceTests.cs`
- `tests/VivoKsu.App.Tests/QuickFlashViewModelTests.cs`

---

### Task 1: Expose Active-Slot Switching Through The Backend

**Files:**

- Modify: `src/VivoKsu.App/Services/IFastbootRsNativeApi.cs`
- Modify: `src/VivoKsu.App/Services/FastbootRsNative.cs`
- Modify: `src/VivoKsu.App/Services/NativeFastbootRsApi.cs`
- Modify: `src/VivoKsu.App/Services/PlatformToolsNativeApi.cs`
- Modify: `src/VivoKsu.App/Services/FastbootRsApiFactory.cs`
- Modify: `src/VivoKsu.App/Services/FastbootRsBackend.cs`
- Test: `tests/VivoKsu.App.Tests/PlatformToolsNativeApiTests.cs`
- Test: `tests/VivoKsu.App.Tests/FastbootRsBackendTests.cs`

**Interfaces:**

- Produces: `void IFastbootRsNativeApi.SetActive(string? serial, string slot)`.
- Produces: `Task FastbootRsBackend.SetActiveAsync(string? serial, string slot, CancellationToken cancellationToken)`.
- Consumes: bundled C export `int fastboot_set_active(const char* serial, const char* slot)`.

- [ ] **Step 1: Add a failing platform-tools command test**

Add this behavior to `PlatformToolsNativeApiTests` using its recording command runner:

```csharp
[Fact]
public void SetActive_runs_fastboot_set_active_for_the_requested_serial()
{
    var runner = new RecordingCommandRunner().Respond("fastboot.exe", string.Empty);
    var api = new PlatformToolsNativeApi(runner, "adb.exe", "fastboot.exe");

    api.SetActive("FAST456", "b");

    var request = Assert.Single(runner.Requests);
    Assert.Equal("fastboot.exe", request.Executable);
    Assert.Equal(["-s", "FAST456", "set_active", "b"], request.Arguments);
}
```

- [ ] **Step 2: Run the platform-tools test and confirm the API is missing**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~SetActive_runs_fastboot_set_active" --no-restore
```

Expected: compile failure because `PlatformToolsNativeApi.SetActive` does not exist.

- [ ] **Step 3: Add a failing backend forwarding test**

Extend the test fake with a captured slot and add:

```csharp
[Fact]
public async Task SetActiveAsync_forwards_serial_and_slot_to_native_api()
{
    var native = new FakeNativeApi(string.Empty);
    var backend = new FastbootRsBackend(native);

    await backend.SetActiveAsync("FAST456", "b", CancellationToken.None);

    Assert.Equal(("FAST456", "b"), native.SetActiveRequest);
}
```

Add this capture to the existing `FakeNativeApi`:

```csharp
public (string? Serial, string Slot)? SetActiveRequest { get; private set; }
public void SetActive(string? serial, string slot) => SetActiveRequest = (serial, slot);
```

- [ ] **Step 4: Implement the native and fallback boundary**

Add a default interface method so existing test fakes continue to compile until they need slot assertions:

```csharp
void SetActive(string? serial, string slot) =>
    throw new NotSupportedException("当前 native 实现不支持切换活动槽位。");
```

Add the P/Invoke:

```csharp
[DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
internal static extern int fastboot_set_active(
    [MarshalAs(UnmanagedType.LPUTF8Str)] string? serial,
    [MarshalAs(UnmanagedType.LPUTF8Str)] string slot);
```

Implement native, platform-tools, composite, and backend forwarding:

```csharp
public void SetActive(string? serial, string slot)
{
    EnsureInitialized();
    ThrowForNativeError(FastbootRsNative.fastboot_set_active(serial, slot), "切换活动槽位");
}

public void SetActive(string? serial, string slot) =>
    Run(fastbootExecutable, WithSerial(serial, "set_active", slot));

public Task SetActiveAsync(string? serial, string slot, CancellationToken cancellationToken) =>
    Task.Run(() => nativeApi.SetActive(serial, slot), cancellationToken);
```

`FastbootRsApiWithPlatformDeviceDiscovery.SetActive` must forward to `nativeApi` just like flash and Fastboot reboot.

- [ ] **Step 5: Run both focused test classes**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~PlatformToolsNativeApiTests|FullyQualifiedName~FastbootRsBackendTests" --no-restore
```

Expected: all selected tests pass.

- [ ] **Step 6: Review checkpoint**

Verify the C# P/Invoke name exactly matches `fastboot_set_active` in `third_party/fastboot-rs-main-correct/fastboot-rs-main/fastboot-exe/src/lib.rs`, then proceed without creating a Git commit.

---

### Task 2: Add The Preset And Execution Models

**Files:**

- Modify: `src/VivoKsu.App/Models/QuickFlashPartition.cs`
- Create: `src/VivoKsu.App/Models/QuickFlashRequest.cs`
- Create: `src/VivoKsu.App/ViewModels/QuickFlashPresetItemViewModel.cs`
- Modify: `src/VivoKsu.App/ViewModels/QuickFlashViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/QuickFlashViewModelTests.cs`

**Interfaces:**

- Produces: `QuickFlashRequest(QuickFlashPartition Partition, FlashImageInfo Image)`.
- Produces: `QuickFlashOptions(FastbootTarget Target, bool WaitForDevice, bool FlashBothSlots, bool SwitchSlotAfterFlash, bool AutoReboot)`.
- Produces: `QuickFlashExecutionPlan(IReadOnlyList<QuickFlashRequest> Requests, QuickFlashOptions Options)`.
- Produces: `QuickFlashPresetItemViewModel` with `Partition`, `DisplayName`, `SelectedImage`, `ImagePath`, and `HasImage`.
- Produces: ordered `IReadOnlyList<QuickFlashPresetItemViewModel> QuickFlashViewModel.Presets` and four option properties.

- [ ] **Step 1: Add a failing preset-order and defaults test**

```csharp
[Fact]
public void Presets_and_flash_options_match_the_compact_reference_layout()
{
    var viewModel = CreateViewModel();

    Assert.Equal(
        [
            QuickFlashPartition.System,
            QuickFlashPartition.SystemExt,
            QuickFlashPartition.Product,
            QuickFlashPartition.Odm,
            QuickFlashPartition.Vendor,
            QuickFlashPartition.VendorBoot,
            QuickFlashPartition.Boot,
            QuickFlashPartition.InitBoot
        ],
        viewModel.Presets.Select(item => item.Partition));
    Assert.True(viewModel.AutoReboot);
    Assert.True(viewModel.WaitForDevice);
    Assert.False(viewModel.FlashBothSlots);
    Assert.False(viewModel.SwitchSlotAfterFlash);
    Assert.False(viewModel.CanSwitchSlotAfterFlash);
}
```

Add this constructor helper once in `QuickFlashViewModelTests` and reuse it in later tasks:

```csharp
private static QuickFlashViewModel CreateViewModel()
{
    var logs = new OperationLogService();
    return new QuickFlashViewModel(
        new DeviceSessionViewModel(),
        new QuickFlashService(new FastbootRsBackend(new EmptyNativeApi()), logs),
        logs);
}
```

- [ ] **Step 2: Run the new ViewModel test and observe the missing API**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~Presets_and_flash_options" --no-restore
```

Expected: compile failure for the new enum values and ViewModel properties.

- [ ] **Step 3: Add immutable request records and the row ViewModel**

Create `QuickFlashRequest.cs`:

```csharp
namespace VivoKsu.App.Models;

public sealed record QuickFlashRequest(QuickFlashPartition Partition, FlashImageInfo Image);

public sealed record QuickFlashOptions(
    FastbootTarget Target,
    bool WaitForDevice,
    bool FlashBothSlots,
    bool SwitchSlotAfterFlash,
    bool AutoReboot);

public sealed record QuickFlashExecutionPlan(
    IReadOnlyList<QuickFlashRequest> Requests,
    QuickFlashOptions Options);
```

Create `QuickFlashPresetItemViewModel.cs`:

```csharp
using CommunityToolkit.Mvvm.ComponentModel;
using VivoKsu.App.Models;

namespace VivoKsu.App.ViewModels;

public sealed partial class QuickFlashPresetItemViewModel(
    QuickFlashPartition partition,
    string displayName) : ObservableObject
{
    public QuickFlashPartition Partition { get; } = partition;
    public string DisplayName { get; } = displayName;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ImagePath))]
    [NotifyPropertyChangedFor(nameof(HasImage))]
    private FlashImageInfo? selectedImage;

    public string ImagePath => SelectedImage?.Path ?? string.Empty;
    public bool HasImage => SelectedImage is not null;
}
```

- [ ] **Step 4: Expand the enum and initialize the interleaved collection**

Use these enum values:

```csharp
public enum QuickFlashPartition
{
    System,
    SystemExt,
    Product,
    Odm,
    Vendor,
    VendorBoot,
    Boot,
    InitBoot
}
```

Declare `public IReadOnlyList<QuickFlashPresetItemViewModel> Presets { get; }` and initialize it in the row-major order used by a two-column `UniformGrid`:

```csharp
Presets =
[
    new(QuickFlashPartition.System, "System"),
    new(QuickFlashPartition.SystemExt, "System_ext"),
    new(QuickFlashPartition.Product, "Product"),
    new(QuickFlashPartition.Odm, "Odm"),
    new(QuickFlashPartition.Vendor, "Vendor"),
    new(QuickFlashPartition.VendorBoot, "Vendor_boot"),
    new(QuickFlashPartition.Boot, "Boot"),
    new(QuickFlashPartition.InitBoot, "Init_boot")
];
```

Add defaulted option properties and `CanSwitchSlotAfterFlash`.

- [ ] **Step 5: Add and pass the dependent-slot-state test**

Add:

```csharp
[Fact]
public void Turning_off_dual_slot_also_turns_off_switch_slot()
{
    var viewModel = CreateViewModel();
    viewModel.FlashBothSlots = true;
    viewModel.SwitchSlotAfterFlash = true;

    viewModel.FlashBothSlots = false;

    Assert.False(viewModel.SwitchSlotAfterFlash);
    Assert.False(viewModel.CanSwitchSlotAfterFlash);
}
```

Implement `OnFlashBothSlotsChanged` to clear slot switching and raise `CanSwitchSlotAfterFlash`.

- [ ] **Step 6: Run the focused ViewModel tests**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~QuickFlashViewModelTests" --no-restore
```

Expected: all ViewModel tests pass after adapting old single-image tests only where the public UI contract intentionally changed.

- [ ] **Step 7: Review checkpoint**

Confirm the collection order renders as four reference rows with left/right pairs and that no public string partition input exists.

---

### Task 3: Implement Multi-Image, Dual-Slot, Switch-Slot Flashing

**Files:**

- Modify: `src/VivoKsu.App/Services/QuickFlashService.cs`
- Test: `tests/VivoKsu.App.Tests/QuickFlashServiceTests.cs`

**Interfaces:**

- Consumes: `QuickFlashRequest`, `QuickFlashOptions`, and `FastbootRsBackend.SetActiveAsync` from Tasks 1-2.
- Produces: `Task FlashImagesAsync(DeviceSessionViewModel session, IReadOnlyList<QuickFlashRequest> requests, QuickFlashOptions options, CancellationToken cancellationToken, OperationContext? context = null)`.
- Preserves: existing `FlashAsync` as a one-request compatibility wrapper.
- Preserves: `FlashRootImagesAsync` behavior and signature.

- [ ] **Step 1: Extend the service test fixture with deterministic Fastboot events**

Add a second temporary image path, delete it from `Dispose`, and extend the existing `QuickFlashNativeApi`:

```csharp
public Func<string, string>? GetVarHandler { get; init; }
public string DeviceListing { get; init; } = "FAST123\tfastboot\n";
public string? FailPartition { get; set; }
public List<string> Events { get; } = [];
public List<(string? Serial, string Slot)> SetActiveRequests { get; } = [];
public int DiscoveryCount { get; private set; }

public string ListDevices()
{
    DiscoveryCount++;
    return DeviceListing;
}

public string GetVar(string? serial, string variable) =>
    GetVarHandler?.Invoke(variable)
    ?? (variable == "is-userspace" ? "no" : string.Empty);

public void Flash(string? serial, string partition, string imagePath)
{
    if (string.Equals(partition, FailPartition, StringComparison.Ordinal))
    {
        throw new InvalidOperationException($"failed {partition}");
    }

    LastFlashRequest = (serial, partition, imagePath);
    FlashRequests.Add((serial, partition, imagePath));
    Events.Add($"flash:{partition}");
}

public void SetActive(string? serial, string slot)
{
    SetActiveRequests.Add((serial, slot));
    Events.Add($"set-active:{slot}");
}

public void FastbootReboot(string? serial)
{
    FastbootRebootSerial = serial;
    Events.Add("reboot");
}
```

Add the exact reusable fixture used by the following tests:

```csharp
private sealed record FlashFixture(
    QuickFlashService Service,
    DeviceSessionViewModel Session,
    QuickFlashNativeApi Native,
    FlashImageInfo Image,
    FlashImageInfo SecondImage);

private readonly string secondImagePath = Path.Combine(
    Path.GetTempPath(), $"vivoksu-second-{Guid.NewGuid():N}.bin");

private async Task<FlashFixture> CreateFlashFixtureAsync(
    Func<string, string> getVar,
    string devices = "FAST123\tfastboot\n")
{
    await File.WriteAllBytesAsync(imagePath, [0x01]);
    await File.WriteAllBytesAsync(secondImagePath, [0x02]);
    var native = new QuickFlashNativeApi
    {
        GetVarHandler = getVar,
        DeviceListing = devices
    };
    var service = new QuickFlashService(new FastbootRsBackend(native), new OperationLogService());
    return new FlashFixture(
        service,
        new DeviceSessionViewModel(),
        native,
        await service.InspectImageAsync(imagePath, CancellationToken.None),
        await service.InspectImageAsync(secondImagePath, CancellationToken.None));
}
```

Add `File.Delete(secondImagePath);` to `Dispose` so the exact helper file is removed.

- [ ] **Step 2: Add a failing dual-slot order test**

```csharp
[Fact]
public async Task FlashImagesAsync_writes_each_selected_partition_to_a_then_b()
{
    var fixture = await CreateFlashFixtureAsync(
        getVar: variable => variable.StartsWith("has-slot:", StringComparison.Ordinal) ? "yes" : "no");

    await fixture.Service.FlashImagesAsync(
        fixture.Session,
        [
            new(QuickFlashPartition.Boot, fixture.Image),
            new(QuickFlashPartition.InitBoot, fixture.SecondImage)
        ],
        new(FastbootTarget.Fastboot, true, true, false, false),
        CancellationToken.None);

    Assert.Equal(
        [
            ("FAST123", "boot_a", fixture.Image.Path),
            ("FAST123", "boot_b", fixture.Image.Path),
            ("FAST123", "init_boot_a", fixture.SecondImage.Path),
            ("FAST123", "init_boot_b", fixture.SecondImage.Path)
        ],
        fixture.Native.FlashRequests);
    Assert.Null(fixture.Native.FastbootRebootSerial);
}
```

- [ ] **Step 3: Run the test and observe `FlashImagesAsync` is missing**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~FlashImagesAsync_writes_each" --no-restore
```

Expected: compile failure for the missing service method.

- [ ] **Step 4: Implement whitelist mapping and target discovery strategy**

Expand `ToPartitionName` to the exact eight names. Replace the unconditional polling helper with:

```csharp
private async Task<DeviceSnapshot> ResolveTargetAsync(
    FastbootTarget target,
    bool waitForDevice,
    CancellationToken cancellationToken)
{
    while (true)
    {
        var device = await backend.DiscoverAsync(cancellationToken);
        if (await MatchesTargetAsync(device, target, cancellationToken))
        {
            return device;
        }

        if (!waitForDevice)
        {
            throw new InvalidOperationException($"未检测到匹配的 {ToTargetLabel(target)} 设备。");
        }

        await Task.Delay(TimeSpan.FromSeconds(1), cancellationToken);
    }
}
```

`MatchesTargetAsync` must call `is-userspace` only for a detected Fastboot device.

- [ ] **Step 5: Add a failing complete-preflight test**

```csharp
[Fact]
public async Task Dual_slot_preflight_rejects_an_unsupported_partition_before_any_write()
{
    var fixture = await CreateFlashFixtureAsync(
        getVar: variable => variable == "has-slot:boot" ? "yes" : "no");

    await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.Service.FlashImagesAsync(
        fixture.Session,
        [
            new(QuickFlashPartition.Boot, fixture.Image),
            new(QuickFlashPartition.VendorBoot, fixture.SecondImage)
        ],
        new(FastbootTarget.Fastboot, true, true, false, false),
        CancellationToken.None));

    Assert.Empty(fixture.Native.FlashRequests);
}
```

- [ ] **Step 6: Implement preflight and flash-plan expansion**

Before any flash call:

```csharp
foreach (var request in requests)
{
    var partition = ToPartitionName(request.Partition);
    var hasSlot = await backend.GetVarAsync(device.Serial, $"has-slot:{partition}", cancellationToken);
    if (!IsTrueFastbootValue(hasSlot))
    {
        throw new InvalidOperationException($"设备分区 {partition} 不支持 A/B 双槽刷写。");
    }
}
```

Then expand each request to base or `_a`/`_b` target names and flash sequentially. Reject an empty request list with `ArgumentException`.

- [ ] **Step 7: Add a failing opposite-slot ordering test**

```csharp
[Fact]
public async Task Switch_slot_runs_after_all_flashes_and_before_optional_reboot()
{
    var fixture = await CreateFlashFixtureAsync(getVar: variable => variable switch
    {
        "is-userspace" => "no",
        "current-slot" => "_a",
        _ when variable.StartsWith("has-slot:", StringComparison.Ordinal) => "yes",
        _ => string.Empty
    });

    await fixture.Service.FlashImagesAsync(
        fixture.Session,
        [new(QuickFlashPartition.Boot, fixture.Image)],
        new(FastbootTarget.Fastboot, true, true, true, true),
        CancellationToken.None);

    Assert.Equal(["flash:boot_a", "flash:boot_b", "set-active:b", "reboot"], fixture.Native.Events);
}
```

- [ ] **Step 8: Implement current-slot normalization, opposite-slot activation, and optional reboot**

Normalize only these values:

```csharp
private static string NormalizeCurrentSlot(string value) => value.Trim().ToLowerInvariant() switch
{
    "a" or "_a" => "a",
    "b" or "_b" => "b",
    _ => throw new InvalidOperationException("无法确定设备当前活动槽位。")
};
```

Read `current-slot` during preflight, derive `a -> b` or `b -> a`, call `SetActiveAsync` only after all flashes, then call `FastbootRebootAsync` only when `AutoReboot` is true.

- [ ] **Step 9: Add failure and no-wait regression tests**

The dual-slot test from Step 2 already proves `AutoReboot=false` produces no reboot. Add the failure test:

```csharp
[Fact]
public async Task Flash_failure_prevents_slot_switch_and_reboot()
{
    var fixture = await CreateFlashFixtureAsync(variable => variable switch
    {
        "is-userspace" => "no",
        "current-slot" => "a",
        _ when variable.StartsWith("has-slot:", StringComparison.Ordinal) => "yes",
        _ => string.Empty
    });
    fixture.Native.FailPartition = "boot_b";

    await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.Service.FlashImagesAsync(
        fixture.Session,
        [new(QuickFlashPartition.Boot, fixture.Image)],
        new(FastbootTarget.Fastboot, true, true, true, true),
        CancellationToken.None));

    Assert.Empty(fixture.Native.SetActiveRequests);
    Assert.Null(fixture.Native.FastbootRebootSerial);
}
```

Add the no-wait test:

```csharp
[Fact]
public async Task Wait_disabled_checks_for_a_matching_device_once()
{
    var fixture = await CreateFlashFixtureAsync(_ => string.Empty, devices: string.Empty);
    using var cancellation = new CancellationTokenSource(TimeSpan.FromMilliseconds(250));

    await Assert.ThrowsAsync<InvalidOperationException>(() => fixture.Service.FlashImagesAsync(
        fixture.Session,
        [new(QuickFlashPartition.Boot, fixture.Image)],
        new(FastbootTarget.Fastboot, false, false, false, false),
        cancellation.Token));

    Assert.Equal(1, fixture.Native.DiscoveryCount);
}
```

- [ ] **Step 10: Preserve single-image and ROOT compatibility wrappers**

Make existing `FlashAsync` call `FlashImagesAsync` with:

```csharp
new QuickFlashOptions(target, WaitForDevice: true, FlashBothSlots: false,
    SwitchSlotAfterFlash: false, AutoReboot: true)
```

Keep `FlashRootImagesAsync` unchanged except for shared private helper reuse that does not alter its existing always-reboot semantics.

- [ ] **Step 11: Run the entire service test class**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~QuickFlashServiceTests" --no-restore
```

Expected: dual-slot, preflight, switching, waiting, cancellation, existing single flash, and ROOT tests all pass.

- [ ] **Step 12: Review checkpoint**

Read the final method top-to-bottom and verify no `FlashAsync` call can occur before every selected partition and optional current slot have passed preflight.

---

### Task 4: Replace Single Selection With Snapshot-Based Preset Commands

**Files:**

- Modify: `src/VivoKsu.App/ViewModels/QuickFlashViewModel.cs`
- Modify: `src/VivoKsu.App/ViewModels/QuickFlashPresetItemViewModel.cs`
- Test: `tests/VivoKsu.App.Tests/QuickFlashViewModelTests.cs`

**Interfaces:**

- Consumes: `QuickFlashService.FlashImagesAsync` from Task 3.
- Produces: `IAsyncRelayCommand<QuickFlashPresetItemViewModel?> BrowsePresetImageCommand`.
- Produces: `IRelayCommand<QuickFlashPresetItemViewModel?> RequestPresetFlashCommand`.
- Produces: `IRelayCommand RequestBatchFlashCommand`.
- Produces: `QuickFlashExecutionPlan? PendingPlan` and derived `ConfirmationSummary`.
- Preserves: `PreparePatchedImage(FlashImageInfo image, QuickFlashPartition partition)`.

- [ ] **Step 1: Add a failing batch-snapshot test**

```csharp
[Fact]
public void RequestBatchFlash_snapshots_only_rows_with_images()
{
    var viewModel = CreateViewModel();
    Find(viewModel, QuickFlashPartition.Boot).SelectedImage = new("C:\\images\\boot.img", 10);
    Find(viewModel, QuickFlashPartition.VendorBoot).SelectedImage = new("C:\\images\\vendor_boot.bin", 20);

    viewModel.RequestBatchFlashCommand.Execute(null);
    Find(viewModel, QuickFlashPartition.Boot).SelectedImage = null;

    Assert.True(viewModel.IsConfirmationVisible);
    Assert.Equal(
        [QuickFlashPartition.VendorBoot, QuickFlashPartition.Boot],
        viewModel.PendingPlan!.Requests.Select(request => request.Partition));
    Assert.Equal(2, viewModel.PendingPlan.Requests.Count);
}
```

Use an explicit literal expected order matching the actual preset order; do not derive it from production helpers.

Add this test-only selector once in `QuickFlashViewModelTests`:

```csharp
private static QuickFlashPresetItemViewModel Find(
    QuickFlashViewModel viewModel,
    QuickFlashPartition partition) =>
    Assert.Single(viewModel.Presets, item => item.Partition == partition);
```

- [ ] **Step 2: Run the test and observe missing batch APIs**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~RequestBatchFlash_snapshots" --no-restore
```

Expected: compile failure for `RequestBatchFlashCommand` and `PendingPlan`.

- [ ] **Step 3: Subscribe to row image changes and implement command invalidation**

In the constructor, subscribe each row's `PropertyChanged`. On `SelectedImage` changes, notify batch, row, and confirmation commands. Unsubscribe is unnecessary because rows and the parent share the same lifetime.

Implement:

```csharp
private QuickFlashExecutionPlan CreatePlan(IEnumerable<QuickFlashPresetItemViewModel> items)
{
    var requests = items
        .Where(item => item.SelectedImage is not null)
        .Select(item => new QuickFlashRequest(item.Partition, item.SelectedImage!))
        .ToArray();
    return new QuickFlashExecutionPlan(requests, new QuickFlashOptions(
        SelectedTarget,
        WaitForDevice,
        FlashBothSlots,
        SwitchSlotAfterFlash,
        AutoReboot));
}
```

Store this immutable plan before showing confirmation.

- [ ] **Step 4: Add and pass the row-only snapshot test**

```csharp
[Fact]
public void RequestPresetFlash_snapshots_only_the_requested_row()
{
    var viewModel = CreateViewModel();
    var boot = Find(viewModel, QuickFlashPartition.Boot);
    boot.SelectedImage = new("C:\\images\\boot.img", 10);
    Find(viewModel, QuickFlashPartition.VendorBoot).SelectedImage = new("C:\\images\\vendor.img", 20);

    viewModel.RequestPresetFlashCommand.Execute(boot);

    Assert.Equal(QuickFlashPartition.Boot, Assert.Single(viewModel.PendingPlan!.Requests).Partition);
}
```

- [ ] **Step 5: Implement per-row browsing**

Use one parameterized async command. The `OpenFileDialog` remains:

```csharp
Filter = "Android image (*.img;*.bin)|*.img;*.bin",
CheckFileExists = true,
Multiselect = false,
Title = $"选择 {item.DisplayName} 镜像"
```

After selection, call `InspectImageAsync` and assign only `item.SelectedImage`.

- [ ] **Step 6: Execute the frozen plan through the coordinator**

`ConfirmFlashAsync` must copy `PendingPlan` to a local, close and clear confirmation, set active state, and call:

```csharp
await coordinator.RunAsync(
    OperationKind.Flashing,
    $"正在刷写 {plan.Requests.Count} 个预设分区",
    (context, token) => quickFlash.FlashImagesAsync(
        session, plan.Requests, plan.Options, token, context));
```

Keep the direct non-coordinator cancellation path for tests and constructor compatibility.

- [ ] **Step 7: Adapt `PreparePatchedImage` without changing callers**

Find the row by enum and set its image:

```csharp
var preset = Presets.Single(item => item.Partition == partition);
preset.SelectedImage = image;
SelectedTarget = FastbootTarget.Fastboot;
PendingPlan = null;
IsConfirmationVisible = false;
```

- [ ] **Step 8: Add confirmation-summary assertions**

Test a dual-slot, switch-slot, no-reboot plan and assert the summary contains the literal user-facing fragments `2 个分区`, `双槽`, `切换槽位`, and `不自动重启`.

- [ ] **Step 9: Run the full ViewModel test class**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~QuickFlashViewModelTests" --no-restore
```

Expected: all tests pass, including existing cancellation and ROOT handoff behavior.

- [ ] **Step 10: Review checkpoint**

Verify changing rows or options after confirmation cannot mutate `PendingPlan`, and verify every command is disabled while `IsFlashOperationActive` is true.

---

### Task 5: Build The Compact Preset Grid UI

**Files:**

- Modify: `src/VivoKsu.App/App.xaml`
- Modify: `src/VivoKsu.App/MainWindow.xaml:238-312`

**Interfaces:**

- Consumes: `QuickFlashViewModel.Presets`, option properties, commands, active state, pending summary, and existing `SelectedTarget`.
- Produces: no new backend behavior; this task binds the tested ViewModel contract.

- [ ] **Step 1: Run a baseline WPF build before editing XAML**

Run:

```powershell
dotnet build src\VivoKsu.App\VivoKsu.App.csproj -c Debug --no-restore
```

Expected: build succeeds, establishing that subsequent XAML failures belong to this task.

- [ ] **Step 2: Replace the large split form with the top action bar**

Keep page title and status. Inside the panel, build one top row with:

```xml
<Button Content="开始刷入"
        Style="{StaticResource SignalButtonStyle}"
        Command="{Binding QuickFlash.RequestBatchFlashCommand}"/>
<ComboBox Width="126"
          Style="{StaticResource SelectBoxStyle}"
          SelectedValuePath="Tag"
          SelectedValue="{Binding QuickFlash.SelectedTarget, Mode=TwoWay}">
  <ComboBoxItem Content="Fastboot" Tag="{x:Static models:FastbootTarget.Fastboot}"/>
  <ComboBoxItem Content="fastbootd" Tag="{x:Static models:FastbootTarget.Fastbootd}"/>
</ComboBox>
<CheckBox Content="自动重启" IsChecked="{Binding QuickFlash.AutoReboot, Mode=TwoWay}"/>
<CheckBox Content="等待 FB 设备" IsChecked="{Binding QuickFlash.WaitForDevice, Mode=TwoWay}"/>
<CheckBox Content="双刷入双槽" IsChecked="{Binding QuickFlash.FlashBothSlots, Mode=TwoWay}"/>
<CheckBox Content="刷完切槽"
          IsChecked="{Binding QuickFlash.SwitchSlotAfterFlash, Mode=TwoWay}"
          IsEnabled="{Binding QuickFlash.CanSwitchSlotAfterFlash}"/>
```

Use the current signal color rather than copying the reference's purple accent.

- [ ] **Step 3: Add the two-column, four-row preset body**

Use one `ItemsControl` with a two-column `UniformGrid` so item order remains deterministic:

```xml
<ItemsControl ItemsSource="{Binding QuickFlash.Presets}">
  <ItemsControl.ItemsPanel>
    <ItemsPanelTemplate><UniformGrid Columns="2"/></ItemsPanelTemplate>
  </ItemsControl.ItemsPanel>
  <ItemsControl.ItemTemplate>
    <DataTemplate>
      <Grid Margin="0,6,18,6">
        <Grid.ColumnDefinitions>
          <ColumnDefinition Width="88"/>
          <ColumnDefinition Width="*"/>
          <ColumnDefinition Width="Auto"/>
          <ColumnDefinition Width="Auto"/>
        </Grid.ColumnDefinitions>
        <TextBlock Text="{Binding DisplayName}" VerticalAlignment="Center"/>
        <Border Grid.Column="1" Height="42" BorderBrush="{StaticResource EdgeBrush}" BorderThickness="1" CornerRadius="5" Padding="12,0">
          <TextBlock Text="{Binding ImagePath}" VerticalAlignment="Center" TextTrimming="CharacterEllipsis"/>
        </Border>
        <Button Grid.Column="2" Content="文件"
                Style="{StaticResource ToolButtonStyle}"
                Command="{Binding DataContext.QuickFlash.BrowsePresetImageCommand, RelativeSource={RelativeSource AncestorType={x:Type Window}}}"
                CommandParameter="{Binding}"/>
        <Button Grid.Column="3" Content="刷入"
                Style="{StaticResource ToolButtonStyle}"
                Command="{Binding DataContext.QuickFlash.RequestPresetFlashCommand, RelativeSource={RelativeSource AncestorType={x:Type Window}}}"
                CommandParameter="{Binding}"/>
      </Grid>
    </DataTemplate>
  </ItemsControl.ItemTemplate>
</ItemsControl>
```

Apply consistent 5-7 px radii, 40-42 px control height, readable 11-12 px compact labels, and no nested card surfaces.

- [ ] **Step 4: Replace the old confirmation copy with the frozen summary**

Bind the confirmation description to `QuickFlash.ConfirmationSummary`. Preserve explicit cancel and confirm commands. The confirmation must remain below the panel and must not shift controls within the preset grid.

- [ ] **Step 5: Preserve active-operation cancellation state**

Show `取消当前操作` in the action bar only while `IsFlashOperationActive` is true. Hide or disable start and every row command through ViewModel `CanExecute`; do not add duplicate `IsEnabled` bindings that can conflict with command state.

- [ ] **Step 6: Build after XAML replacement**

Run:

```powershell
dotnet build src\VivoKsu.App\VivoKsu.App.csproj -c Debug --no-restore
```

Expected: zero XAML compiler errors and zero C# compiler errors.

- [ ] **Step 7: Visual preflight checkpoint**

Inspect the live WPF page and verify:

- all eight rows fit without overlap at the current window minimum size;
- the longest label `Vendor_boot` fits its fixed column;
- selected paths ellipsize instead of expanding rows;
- option text does not wrap;
- `刷完切槽` visibly disables when dual-slot is off;
- controls use one radius scale and the current application accent;
- the right-side log and lower-left device status are unchanged.

---

### Task 6: Full Regression, Visual Verification, And Release Package

**Files:**

- Modify: `docs/superpowers/specs/2026-08-11-quick-flash-preset-grid-dual-slot-design.md`
- Generate: `artifacts/release/VivoKsu-win-x64/`
- Generate: `artifacts/release/VivoKsu-win-x64.zip`
- Generate: `artifacts/verification/quick-flash-preset-grid.png`

**Interfaces:**

- Consumes: completed implementation from Tasks 1-5.
- Produces: tested Release build, current self-contained package, and visual evidence.

- [ ] **Step 1: Run all quick-flash and backend tests together**

Run:

```powershell
dotnet test tests\VivoKsu.App.Tests\VivoKsu.App.Tests.csproj --filter "FullyQualifiedName~QuickFlash|FullyQualifiedName~FastbootRsBackend|FullyQualifiedName~PlatformToolsNativeApi" --no-restore
```

Expected: zero failures.

- [ ] **Step 2: Run the full solution suite**

Run:

```powershell
dotnet test VivoKsu.slnx
```

Expected: zero failures and no skipped regression caused by the quick-flash rewrite.

- [ ] **Step 3: Build Release**

Run:

```powershell
dotnet build VivoKsu.slnx -c Release
```

Expected: zero warnings and zero errors.

- [ ] **Step 4: Publish the self-contained package**

Ensure no running `VivoKsu.App` process owns the release files, then run:

```powershell
& .\scripts\Publish-Release.ps1
```

Expected: publish and archive creation succeed; bundled `scrcpy`, platform-tools, ROOT resources, and APK assets remain present.

- [ ] **Step 5: Launch the published EXE and capture a screenshot**

Create `artifacts/verification` if absent, launch the published executable normally, navigate to `快速刷写`, and capture the visible application window to:

```text
artifacts/verification/quick-flash-preset-grid.png
```

Use `functions.view_image` to inspect the screenshot at original detail. Check row alignment, option wrapping, confirmation placement, log width, and minimum-window clipping. Fix and repeat until the screenshot passes the Task 5 visual preflight.

- [ ] **Step 6: Startup smoke test the final published EXE**

Run the published executable for at least three seconds. Confirm it remains alive, then terminate only that test process by its captured PID.

- [ ] **Step 7: Perform read-only device verification**

When a Fastboot device is connected, run only read-only checks:

```powershell
fastboot devices
fastboot getvar current-slot
fastboot getvar has-slot:boot
```

Do not flash or call `set_active` during automated verification. Destructive behavior is covered by recording fakes.

- [ ] **Step 8: Update implementation status and perform final review**

Append the verified test count, build result, publish result, screenshot path, and any device-read evidence to the design spec. Re-read all changed quick-flash code for immutable confirmation snapshots, preflight-before-write ordering, stop-on-first-failure behavior, and no unrelated UI changes.

- [ ] **Step 9: Final no-Git checkpoint**

List the changed source, tests, design, plan, screenshot, and release paths. Do not initialize Git or claim a commit was created.
