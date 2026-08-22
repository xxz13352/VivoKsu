using System.Globalization;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public static class FastbootPartitionTableParser
{
    public static PartitionSnapshot Parse(string serial, string output)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(serial);
        var sizes = new Dictionary<string, long?>(StringComparer.OrdinalIgnoreCase);
        var activeSlot = string.Empty;

        foreach (var sourceLine in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            var line = NormalizeLine(sourceLine);
            if (line.StartsWith("current-slot:", StringComparison.OrdinalIgnoreCase))
            {
                activeSlot = NormalizeSlot(line["current-slot:".Length..]);
                continue;
            }

            const string sizePrefix = "partition-size:";
            if (!line.StartsWith(sizePrefix, StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            var item = line[sizePrefix.Length..];
            var separator = item.LastIndexOf(':');
            if (separator <= 0)
            {
                continue;
            }

            var name = item[..separator].Trim();
            if (!string.IsNullOrWhiteSpace(name))
            {
                sizes[name] = ParseSize(item[(separator + 1)..]);
            }
        }

        var partitions = sizes
            .OrderBy(pair => pair.Key, StringComparer.OrdinalIgnoreCase)
            .Select(pair => new DevicePartition(
                pair.Key,
                pair.Key,
                pair.Value,
                GetSlot(pair.Key),
                false,
                PartitionRiskPolicy.IsHighRisk(pair.Key),
                true))
            .ToArray();

        return new PartitionSnapshot(serial, PartitionTransportKind.Fastboot, activeSlot, partitions);
    }

    private static string NormalizeLine(string sourceLine)
    {
        var line = sourceLine.Trim();
        const string bootloaderPrefix = "(bootloader)";
        if (line.StartsWith(bootloaderPrefix, StringComparison.OrdinalIgnoreCase))
        {
            line = line[bootloaderPrefix.Length..].TrimStart();
        }

        return line.StartsWith("INFO", StringComparison.OrdinalIgnoreCase)
            ? line["INFO".Length..].TrimStart()
            : line;
    }

    private static long? ParseSize(string value)
    {
        var trimmed = value.Trim();
        if (trimmed.StartsWith("0x", StringComparison.OrdinalIgnoreCase) &&
            long.TryParse(trimmed[2..], NumberStyles.AllowHexSpecifier, CultureInfo.InvariantCulture, out var hexadecimal))
        {
            return hexadecimal;
        }

        return long.TryParse(trimmed, NumberStyles.Integer, CultureInfo.InvariantCulture, out var decimalValue)
            ? decimalValue
            : null;
    }

    private static string GetSlot(string partitionName) => partitionName.EndsWith("_a", StringComparison.OrdinalIgnoreCase)
        ? "a"
        : partitionName.EndsWith("_b", StringComparison.OrdinalIgnoreCase)
            ? "b"
            : string.Empty;

    private static string NormalizeSlot(string value) => value.Trim().TrimStart('_').ToLowerInvariant();

}
