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

    private readonly IFastbootCliRunner cliRunner;

    public FastbootPartitionService(IFastbootCliRunner cliRunner)
    {
        this.cliRunner = cliRunner;
    }

    public async Task<FastbootPartitionTableSnapshot> ReadAsync(string serial, CancellationToken cancellationToken)
    {
        var activeSlot = Normalize(await ReadVarAsync(serial, "current-slot", cancellationToken));
        var userspace = Normalize(await ReadVarAsync(serial, "is-userspace", cancellationToken));
        var partitions = new List<FastbootPartitionInfo>(SupportedPartitions.Length);
        var anyRead = false;

        foreach (var partition in SupportedPartitions)
        {
            var size = ParseSize(await ReadVarAsync(serial, $"partition-size:{partition.Name}", cancellationToken));
            anyRead |= size is not null;
            partitions.Add(new FastbootPartitionInfo(
                partition.Name,
                partition.Purpose,
                size is null ? "--" : FormatSize(size.Value),
                size is null ? "未读取" : "已读取"));
        }

        // 一个分区都没读到(getvar 全部失败或全部为空):设备不可达或该
        // fastboot 不支持 getvar。绝不能返回一张全 "--" 的表让上层误报
        // “分区表已更新”——用户无法区分“设备不可达”和“正常读取”。
        if (!anyRead)
        {
            throw new InvalidOperationException("无法读取分区信息：getvar 全部失败，设备可能已断开或该 fastboot 不支持 getvar。");
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
            return await cliRunner.GetVarAsync(serial, name, cancellationToken);
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
