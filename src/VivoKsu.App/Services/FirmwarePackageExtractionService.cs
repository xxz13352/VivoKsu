using System.IO.Compression;
using System.IO;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class FirmwarePackageExtractionService
{
    private static readonly TimeSpan StagingRetention = TimeSpan.FromHours(2);

    private static string StagingRoot => Path.Combine(Path.GetTempPath(), "VivoKsu", "firmware-stage");

    public async Task<FirmwarePackageExtractionResult> ExtractAsync(
        FirmwarePackageInspection package,
        string entryPath,
        CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(package);
        cancellationToken.ThrowIfCancellationRequested();

        if (!package.ManagedImageEntries.Contains(entryPath, StringComparer.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException("该镜像不在受控的快速刷写分区范围内。");
        }

        SweepStaleStagingDirectories();

        var partition = ToPartition(Path.GetFileNameWithoutExtension(entryPath));
        var stagingDirectory = Path.Combine(
            StagingRoot,
            Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(stagingDirectory);

        try
        {
            using var archive = ZipFile.OpenRead(package.PackagePath);
            var entry = archive.Entries.SingleOrDefault(candidate =>
                string.Equals(candidate.FullName, entryPath, StringComparison.OrdinalIgnoreCase));

            if (entry is null || string.IsNullOrWhiteSpace(entry.Name))
            {
                throw new InvalidDataException("固件包中未找到指定镜像。");
            }

            var destinationPath = Path.Combine(
                stagingDirectory,
                $"{ToPartitionName(partition)}-{Guid.NewGuid():N}.img");

            await using (var source = entry.Open())
            await using (var destination = new FileStream(destinationPath, FileMode.CreateNew, FileAccess.Write, FileShare.None, 81920, useAsync: true))
            {
                await source.CopyToAsync(destination, cancellationToken);
            }

            var file = new FileInfo(destinationPath);
            if (file.Length == 0)
            {
                throw new InvalidDataException("提取出的镜像为空。");
            }

            return new FirmwarePackageExtractionResult(
                new FlashImageInfo(file.FullName, file.Length),
                partition);
        }
        catch
        {
            Directory.Delete(stagingDirectory, recursive: true);
            throw;
        }
    }

    private static QuickFlashPartition ToPartition(string partitionName) => partitionName.ToLowerInvariant() switch
    {
        "boot" => QuickFlashPartition.Boot,
        "init_boot" => QuickFlashPartition.InitBoot,
        "vendor_boot" => QuickFlashPartition.VendorBoot,
        "lk" => QuickFlashPartition.Lk,
        _ => throw new InvalidOperationException("该镜像不支持快速刷写。")
    };

    private static string ToPartitionName(QuickFlashPartition partition) => partition switch
    {
        QuickFlashPartition.Boot => "boot",
        QuickFlashPartition.InitBoot => "init_boot",
        QuickFlashPartition.VendorBoot => "vendor_boot",
        QuickFlashPartition.Lk => "lk",
        _ => throw new ArgumentOutOfRangeException(nameof(partition), partition, "不支持的快速刷写分区。")
    };

    // Successful extractions intentionally keep the staged image for later flashing,
    // so this sweep bounds the temporary disk usage instead of leaking indefinitely.
    private static void SweepStaleStagingDirectories()
    {
        try
        {
            if (!Directory.Exists(StagingRoot))
            {
                return;
            }

            var cutoff = DateTime.UtcNow - StagingRetention;
            foreach (var directory in Directory.GetDirectories(StagingRoot))
            {
                try
                {
                    if (Directory.GetLastWriteTimeUtc(directory) < cutoff)
                    {
                        Directory.Delete(directory, recursive: true);
                    }
                }
                catch
                {
                    // A locked staging directory is swept on a later run.
                }
            }
        }
        catch
        {
            // Best effort; extraction proceeds regardless.
        }
    }
}
