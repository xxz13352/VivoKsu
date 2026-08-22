namespace VivoKsu.App.Models;

public sealed record FirmwarePackageExtractionResult(
    FlashImageInfo Image,
    QuickFlashPartition Partition);
