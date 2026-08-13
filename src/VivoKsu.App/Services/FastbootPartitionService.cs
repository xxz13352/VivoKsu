using System.Globalization;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class FastbootPartitionService
{
    private static readonly (string Name, string Purpose)[] SupportedPartitions =
    [
        ("boot", "启动镜像"),
        ("init_boot", "GKI 初始启动镜像"),
        ("vendor_boot", "Vendor Boot 镜像")
    ];

    private readonly FastbootRsBackend backend;

    public FastbootPartitionService(FastbootRsBackend backend)
    {
        this.backend = backend;
    }

    public async Task<FastbootPartitionTableSnapshot> ReadAsync(string serial, CancellationToken cancellationToken)
    {
        var activeSlot = Normalize(await ReadVarAsync(serial, "current-slot", cancellationToken));
        var userspace = Normalize(await ReadVarAsync(serial, "is-userspace", cancellationToken));
        var partitions = new List<FastbootPartitionInfo>(SupportedPartitions.Length);

        foreach (var partition in SupportedPartitions)
        {
            var size = ParseSize(await ReadVarAsync(serial, $"partition-size:{partition.Name}", cancellationToken));
            partitions.Add(new FastbootPartitionInfo(
                partition.Name,
                partition.Purpose,
                size is null ? "--" : FormatSize(size.Value),
                size is null ? "未读取" : "已读取"));
        }

        return new FastbootPartitionTableSnapshot(
            string.IsNullOrWhiteSpace(activeSlot) ? "--" : activeSlot.TrimStart('_'),
            userspace.Equals("yes", StringComparison.OrdinalIgnoreCase) || userspace.Equals("true", StringComparison.OrdinalIgnoreCase)
                ? "fastbootd"
                : "fastboot",
            partitions);
    }

    private async Task<string> ReadVarAsync(string serial, string name, CancellationToken cancellationToken)
    {
        try
        {
            return await backend.GetVarAsync(serial, name, cancellationToken);
        }
        catch (OperationCanceledException)
        {
            throw;
        }
        catch
        {
            return string.Empty;
        }
    }

    private static string Normalize(string value)
    {
        var trimmed = value.Trim();
        var separator = trimmed.LastIndexOf(':');
        return separator >= 0 ? trimmed[(separator + 1)..].Trim() : trimmed;
    }

    private static long? ParseSize(string value)
    {
        var normalized = Normalize(value);
        if (normalized.StartsWith("0x", StringComparison.OrdinalIgnoreCase) &&
            long.TryParse(normalized[2..], NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out var hexadecimal) && hexadecimal > 0)
        {
            return hexadecimal;
        }

        return long.TryParse(normalized, NumberStyles.Integer, CultureInfo.InvariantCulture, out var decimalValue) && decimalValue > 0
            ? decimalValue
            : null;
    }

    private static string FormatSize(long bytes)
    {
        const long kilobyte = 1024;
        const long megabyte = kilobyte * 1024;
        const long gigabyte = megabyte * 1024;

        if (bytes >= gigabyte)
        {
            return $"{bytes / (double)gigabyte:0.#} GB";
        }

        if (bytes >= megabyte)
        {
            return $"{bytes / (double)megabyte:0.#} MB";
        }

        return bytes >= kilobyte ? $"{bytes / (double)kilobyte:0.#} KB" : $"{bytes} B";
    }
}
