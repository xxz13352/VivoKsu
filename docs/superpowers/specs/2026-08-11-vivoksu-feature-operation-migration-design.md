# VivoKsu Feature Operation Migration Design

## Goal

Migrate the remaining ROOT, file-management, line-flash, and ADB-mirror command entry points to the existing `OperationCoordinator`. Each foreground task must share serial execution, correlated logging, cancellation propagation, the fixed lower-left device status, and the device-monitor pause already used by reboot and quick flash.

The current WPF layout, fixed right-side log pane, bundled tools, device protocol boundary, and ROOT patching algorithms remain unchanged.

## Scope

This increment covers four existing feature areas in migration order:

1. ROOT manager installation, KMI detection, patching, and the all-in-one ROOT flow.
2. Remote file listing, upload, download, delete, and APK installation.
3. Manual Fastboot partition-table reads, Vivo firmware inspection, and managed-image extraction.
4. Manual ADB-mirror launch plus automatic restart gating while another foreground task is active.

Stopping an existing mirror process remains immediate and is intentionally not queued. Reopening page navigation, selecting a local directory, and handing a completed image to quick flash remain local UI actions.

## Common Execution Boundary

Each migrated view model receives an optional `IOperationCoordinator` constructor argument. `AppComposition` passes the one shared instance; existing tests and callers without that argument retain their current direct behavior.

In the coordinator path, a command calls:

```csharp
await coordinator.RunAsync(kind, title, async (context, token) =>
{
    // Service calls receive token and context.
});
```

The coordinator owns start, terminal session status, and correlated terminal logging. Feature services and view models only report intermediate stages through `OperationContext`, propagate cancellation, update their own page data, and rethrow failures for the coordinator to record.

When no coordinator is supplied, current page-level session updates and log messages remain available for constructor compatibility.

## ROOT Flow

`RootViewModel` uses the coordinator for:

- image inspection (`Hashing`);
- manager APK installation (`Installing`);
- KMI resolution (`Discovering`);
- manual init_boot and optional vendor_boot patching (`Hashing`);
- the automatic sequence (`Installing` -> `Hashing` -> `Rebooting` -> `Flashing`).

`VivoKsuDevicePatchService` and `VivoVendorBootProcessor` gain an optional `OperationContext` parameter. They report meaningful stages before resource extraction, ADB upload, remote processing, and pulling the generated image. `QuickFlashService.FlashRootImagesAsync` already supports the same context and remains the sole fastboot flashing implementation.

The automatic ROOT flow runs as one coordinator task, so the device monitor cannot rediscover the device between manager installation, ADB-side patching, bootloader reboot, and fastboot flashing.

## File Management

All ADB commands from `FileManagerViewModel` use the shared coordinator:

- remote directory reads use `Discovering`;
- upload and download use `Transferring`;
- remote deletion uses `Transferring`;
- APK installation uses `Installing`.

`AdbFileService` accepts an optional `OperationContext` for each mutating transfer/install/delete operation. In the coordinated path it reports stages rather than producing uncorrelated duplicate lifecycle logs. Page collections refresh within the same coordinator delegate, using the operation token, so a refresh cannot race the device monitor.

## Line Flash

Manual partition-table reads use a `Discovering` coordinator task. The automatic partition-table refresh remains background work and still exits immediately when a foreground task is busy.

Firmware ZIP inspection and managed image extraction use `Hashing` coordinator tasks because they currently update the shared device-status surface. They remain local file operations and never perform arbitrary partition flashing; extracted images are still handed to the existing quick-flash whitelist.

## ADB Mirror

Manual mirror launch is a short `Mirroring` coordinator operation. A successful launch releases the foreground lock immediately while the independent scrcpy process remains active.

`MirrorService` receives the shared coordinator as an optional dependency and refuses automatic reconciliation/restart while another foreground operation is busy. After a foreground operation completes, the monitor's compensating refresh invokes the existing mirror reconciliation path. Manual and shutdown stops bypass the queue so they can terminate scrcpy promptly.

## Failure, Cancellation, and Compatibility

- All new coordinator delegates pass their token through to backend and file operations.
- `OperationCanceledException` is rethrown after page cleanup; the coordinator records the cancellation exactly once.
- Feature exceptions are rethrown in the coordinated path; the coordinator owns failed session status and error logging.
- Existing direct paths retain their current failure handling for compatibility tests.
- Page commands expose their existing busy flags; coordinator-backed command predicates also consider `session.IsBusy` where that prevents duplicate clicks.

## Testing

Add coordinator-integration coverage for one representative command in each area:

- ROOT automatic flow retains a single correlated operation through reboot and root-image flash.
- File upload uses the shared coordinator and returns it to idle.
- Manual partition-table refresh uses the coordinator while automatic refresh remains skipped when busy.
- Manual mirror start is queued behind an existing coordinator task; automatic mirror restart does not begin while busy.

Run focused feature tests followed by `dotnet test VivoKsu.slnx`, `dotnet build VivoKsu.slnx -c Release`, and the release publish script. The release ZIP must still include bundled platform-tools, scrcpy, ROOT resources, and APK assets.

## Non-goals

- No page layout, typography, control, or navigation changes.
- No new device protocol or native API boundary.
- No arbitrary partition flashing or changes to the Vivo `vendor_boot` processing algorithm.
- No new visible global cancel button in this increment.

## ROOT Implementation Note

2026-08-11: ROOT image inspection, manager installation, KMI resolution, manual patching, the automatic ROOT flow, and all ADB file-management operations now use the shared operation coordinator. File navigation rolls back when a directory cannot be read. Line flash and mirror migration remain the next increments.
