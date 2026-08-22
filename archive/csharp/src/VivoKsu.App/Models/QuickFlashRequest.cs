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
