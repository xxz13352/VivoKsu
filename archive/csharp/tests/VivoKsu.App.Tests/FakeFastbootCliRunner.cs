using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

/// <summary>测试用 fastboot CLI runner:记录调用、可按变量返回 getvar 值、可注入失败。</summary>
internal sealed class FakeFastbootCliRunner : IFastbootCliRunner
{
    public List<(string Serial, string Partition, string ImagePath)> FlashRequests { get; } = [];
    public (string Partition, string ImagePath)? LastFlash { get; private set; }
    public List<string> Erased { get; } = [];
    public List<string> Rebooted { get; } = [];
    public List<string> SetActiveSlots { get; } = [];
    public List<string> Events { get; } = [];
    public Func<string, string>? GetVarHandler { get; set; }
    public string? FailPartition { get; set; }
    public HashSet<string> MissingPartitions { get; set; } = [];
    public bool IsAvailable => true;

    public Task<string> GetVarAsync(string serial, string variable, CancellationToken cancellationToken) =>
        Task.FromResult(GetVarHandler?.Invoke(variable) ?? (variable == "is-userspace" ? "no" : string.Empty));

    public Task FlashAsync(string serial, string partition, string imagePath, IProgress<double>? progress, CancellationToken cancellationToken)
    {
        if (string.Equals(partition, FailPartition, StringComparison.Ordinal))
        {
            throw new InvalidOperationException($"failed {partition}");
        }

        FlashRequests.Add((serial, partition, imagePath));
        LastFlash = (partition, imagePath);
        Events.Add($"flash:{partition}");
        progress?.Report(1);
        return Task.CompletedTask;
    }

    public Task EraseAsync(string serial, string partition, CancellationToken cancellationToken)
    {
        Erased.Add(partition);
        Events.Add($"erase:{partition}");
        return Task.CompletedTask;
    }

    public Task<bool> PartitionExistsAsync(string serial, string partition, CancellationToken cancellationToken) =>
        Task.FromResult(!MissingPartitions.Contains(partition, StringComparer.OrdinalIgnoreCase));

    public Task RebootAsync(string serial, CancellationToken cancellationToken)
    {
        Rebooted.Add(serial);
        Events.Add("reboot");
        return Task.CompletedTask;
    }

    public Task SetActiveAsync(string serial, string slot, CancellationToken cancellationToken)
    {
        SetActiveSlots.Add(slot);
        Events.Add($"set-active:{slot}");
        return Task.CompletedTask;
    }
}
