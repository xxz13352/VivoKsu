using System.IO.Compression;
using System.IO;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class FirmwarePackageInspector
{
    public Task<FirmwarePackageInspection> InspectAsync(string packagePath, CancellationToken cancellationToken)
    {
        return Task.Run(() => Inspect(packagePath, cancellationToken), cancellationToken);
    }

    private static FirmwarePackageInspection Inspect(string packagePath, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (string.IsNullOrWhiteSpace(packagePath) || !File.Exists(packagePath))
        {
            throw new FileNotFoundException("未找到固件包。", packagePath);
        }

        if (!string.Equals(Path.GetExtension(packagePath), ".zip", StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException("当前仅支持检查 ZIP 格式的 Vivo 固件包。");
        }

        using var archive = ZipFile.OpenRead(packagePath);
        var images = archive.Entries
            .Where(entry => !string.IsNullOrWhiteSpace(entry.Name) && entry.FullName.EndsWith(".img", StringComparison.OrdinalIgnoreCase))
            .Select(entry => entry.FullName)
            .OrderBy(name => name, StringComparer.OrdinalIgnoreCase)
            .ToArray();

        return new FirmwarePackageInspection(
            packagePath,
            Path.GetFileName(packagePath),
            archive.Entries.Count,
            images);
    }
}
