using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public static class PartitionRiskPolicy
{
    private static readonly string[] HighRiskPrefixes =
    [
        "abl", "frp", "gpt", "lk", "metadata", "modemst", "partition", "persist", "preloader", "super", "userdata", "vbmeta", "xbl"
    ];

    public static bool IsHighRisk(string partitionName) => HighRiskPrefixes.Any(prefix =>
    {
        if (partitionName.Length < prefix.Length ||
            !partitionName.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        if (partitionName.Length == prefix.Length)
        {
            return true;
        }

        var next = partitionName[prefix.Length];
        return next is '_' or (>= '0' and <= '9');
    });
}
