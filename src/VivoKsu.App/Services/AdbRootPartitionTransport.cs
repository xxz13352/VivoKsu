using System.Globalization;
using System.Text.RegularExpressions;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class AdbRootPartitionTransport : IPartitionTransport
{
    private const string DiscoverCommand = "for d in /dev/block/by-name /dev/block/bootdevice/by-name /dev/block/platform/*/by-name; do [ -d \"$d\" ] || continue; for p in \"$d\"/*; do [ -e \"$p\" ] || continue; n=${p##*/}; r=$(readlink -f \"$p\") || continue; s=$(blockdev --getsize64 \"$r\" 2>/dev/null); m=0; grep -Fq \" $r \" /proc/mounts && m=1; printf '%s|%s|%s|%s\\n' \"$n\" \"$r\" \"$s\" \"$m\"; done; done";

    private static readonly Regex ValidPartitionNameRegex = new(
        "^[A-Za-z0-9_.-]+$",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    private readonly IAdbRootTransferRunner runner;

    public AdbRootPartitionTransport(IAdbRootTransferRunner runner)
    {
        this.runner = runner;
    }

    public PartitionTransportKind Kind => PartitionTransportKind.AdbRoot;

    public async Task<PartitionSnapshot> DiscoverAsync(string serial, CancellationToken cancellationToken)
    {
        try
        {
            var userId = (await runner.RunRootAsync(serial, "id -u", cancellationToken)).Trim();
            if (!string.Equals(userId, "0", StringComparison.Ordinal))
            {
                throw new InvalidOperationException("ADB 设备未授予 Root 权限。");
            }

            var activeSlot = NormalizeSlot(await runner.RunRootAsync(serial, "getprop ro.boot.slot_suffix", cancellationToken));
            var output = await runner.RunRootAsync(serial, DiscoverCommand, cancellationToken);
            return new PartitionSnapshot(serial, Kind, activeSlot, ParsePartitions(output));
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new PartitionOperationException(Kind, "分区表", "读取", exception);
        }
    }

    public async Task BackupAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(task.OutputPath);
        await ExecuteAsync(serial, task, cancellationToken, "读取", () => runner.CopyFromDeviceAsync(serial, task.DevicePath, task.OutputPath, progress, cancellationToken));
    }

    public async Task WriteAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(task.ImagePath);
        await ExecuteAsync(serial, task, cancellationToken, "写入", () => runner.CopyToDeviceAsync(serial, task.ImagePath, task.DevicePath, progress, cancellationToken));
    }

    public async Task EraseAsync(
        string serial,
        PartitionTask task,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        await ExecuteAsync(serial, task, cancellationToken, "擦除", () => runner.EraseAsync(serial, task.DevicePath, progress, cancellationToken));
    }

    private async Task ExecuteAsync(string serial, PartitionTask task, CancellationToken cancellationToken, string stage, Func<Task> operation)
    {
        try
        {
            await EnsurePartitionUnchangedAsync(serial, task, cancellationToken);
            await operation();
        }
        catch (Exception exception) when (exception is not OperationCanceledException)
        {
            throw new PartitionOperationException(Kind, task.PartitionName, stage, exception);
        }
    }

    private async Task EnsurePartitionUnchangedAsync(string serial, PartitionTask task, CancellationToken cancellationToken)
    {
        // The partition name travels from the device through the discovery output;
        // reject anything that could inject shell metacharacters before using it.
        if (string.IsNullOrEmpty(task.PartitionName) || !ValidPartitionNameRegex.IsMatch(task.PartitionName))
        {
            throw new InvalidOperationException("无效的分区名，已阻止执行。");
        }

        // Re-resolve the by-name symlink right before executing so a partition layout
        // change between discovery and execution cannot target a different device.
        var resolved = (await runner.RunRootAsync(serial, $"readlink -f /dev/block/by-name/{task.PartitionName}", cancellationToken)).Trim();
        if (!string.Equals(resolved, task.DevicePath, StringComparison.Ordinal))
        {
            throw new InvalidOperationException("分区设备路径已变化，请重新读取分区表后再执行。");
        }
    }

    private static IReadOnlyList<DevicePartition> ParsePartitions(string output)
    {
        var partitions = new Dictionary<string, DevicePartition>(StringComparer.OrdinalIgnoreCase);

        foreach (var line in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            var fields = line.Split('|', StringSplitOptions.TrimEntries);
            if (fields.Length != 4 || string.IsNullOrWhiteSpace(fields[0]) || string.IsNullOrWhiteSpace(fields[1]))
            {
                continue;
            }

            var name = fields[0];
            partitions.TryAdd(name, new DevicePartition(
                name,
                fields[1],
                long.TryParse(fields[2], NumberStyles.Integer, CultureInfo.InvariantCulture, out var size) ? size : null,
                GetSlot(name),
                fields[3] == "1",
                PartitionRiskPolicy.IsHighRisk(name),
                true));
        }

        return partitions.Values.OrderBy(partition => partition.Name, StringComparer.OrdinalIgnoreCase).ToArray();
    }

    private static string NormalizeSlot(string value) => value.Trim().TrimStart('_').ToLowerInvariant();

    private static string GetSlot(string partitionName) => partitionName.EndsWith("_a", StringComparison.OrdinalIgnoreCase)
        ? "a"
        : partitionName.EndsWith("_b", StringComparison.OrdinalIgnoreCase)
            ? "b"
            : string.Empty;
}
