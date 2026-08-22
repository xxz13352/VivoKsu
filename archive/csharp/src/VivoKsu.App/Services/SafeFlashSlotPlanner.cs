using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// VIVO 线刷槽位目标规划(纯函数,可单测)。
/// 约定:非槽位分区(has-slot:false)或设备非 A/B 时,目标回退为分区原名,保证安全降级不砖机。
/// </summary>
public static class SafeFlashSlotPlanner
{
    /// <summary>是否需要对设备做槽位探测(has-slot / current-slot)的模式。</summary>
    public static bool IsSlotBasedMode(SafeFlashSlotMode mode) => mode != SafeFlashSlotMode.CurrentSlot;

    /// <summary>按模式与设备信息计算某分区要刷入的目标分区名数组。</summary>
    public static string[] ComputeTargets(
        string partitionName,
        SafeFlashSlotMode mode,
        string? currentSlot,
        bool hasSlot)
    {
        if (!hasSlot)
        {
            return [partitionName];
        }

        return mode switch
        {
            SafeFlashSlotMode.OtherSlot => [TargetForSlot(partitionName, OtherSlot(currentSlot))],
            SafeFlashSlotMode.BothSlots => [TargetForSlot(partitionName, "a"), TargetForSlot(partitionName, "b")],
            _ => [partitionName]
        };
    }

    /// <summary>当前槽的对侧槽位("a"→"b"、"b"→"a"),读不到/异常返回 null。</summary>
    public static string? OtherSlot(string? currentSlot) =>
        currentSlot?.Trim().ToLowerInvariant() switch
        {
            "a" or "_a" => "b",
            "b" or "_b" => "a",
            _ => null
        };

    private static string TargetForSlot(string partitionName, string? slot) =>
        slot is null ? partitionName : $"{partitionName}_{slot}";
}
