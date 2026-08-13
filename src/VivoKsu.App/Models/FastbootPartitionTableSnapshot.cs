namespace VivoKsu.App.Models;

public sealed record FastbootPartitionInfo(
    string Name,
    string Purpose,
    string SizeDisplay,
    string Status);

public sealed record FastbootPartitionTableSnapshot(
    string ActiveSlot,
    string ModeLabel,
    IReadOnlyList<FastbootPartitionInfo> Partitions);
