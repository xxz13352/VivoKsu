using System.IO;

namespace VivoKsu.App.Models;

public sealed record FirmwarePackageInspection(
    string PackagePath,
    string PackageName,
    int EntryCount,
    IReadOnlyList<string> ImageEntries)
{
    private static readonly HashSet<string> ManagedPartitionNames = new(StringComparer.OrdinalIgnoreCase)
    {
        "boot",
        "init_boot",
        "vendor_boot",
        "lk"
    };

    public IReadOnlyList<string> ManagedImageEntries => ImageEntries
        .Where(entry => ManagedPartitionNames.Contains(Path.GetFileNameWithoutExtension(entry)))
        .ToArray();
}
