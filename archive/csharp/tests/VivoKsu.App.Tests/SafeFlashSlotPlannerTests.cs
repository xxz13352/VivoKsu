using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class SafeFlashSlotPlannerTests
{
    [Theory]
    [InlineData("boot", SafeFlashSlotMode.CurrentSlot, "a", true, "boot")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, "a", true, "boot_b")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, "b", true, "boot_a")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, null, true, "boot")]
    [InlineData("boot", SafeFlashSlotMode.OtherSlot, "a", false, "boot")]
    [InlineData("boot", SafeFlashSlotMode.BothSlots, "a", true, "boot_a", "boot_b")]
    [InlineData("boot", SafeFlashSlotMode.BothSlots, "a", false, "boot")]
    public void ComputeTargets_returns_expected_targets(
        string partition, SafeFlashSlotMode mode, string? currentSlot, bool hasSlot, params string[] expected)
    {
        SafeFlashSlotPlanner.ComputeTargets(partition, mode, currentSlot, hasSlot)
            .Should().BeEquivalentTo(expected);
    }

    [Theory]
    [InlineData("a", "b")]
    [InlineData("b", "a")]
    [InlineData("_a", "b")]
    [InlineData(null, null)]
    [InlineData("", null)]
    [InlineData("c", null)]
    public void OtherSlot_maps_a_and_b_only(string? current, string? expected)
    {
        SafeFlashSlotPlanner.OtherSlot(current).Should().Be(expected);
    }

    [Theory]
    [InlineData(SafeFlashSlotMode.CurrentSlot, false)]
    [InlineData(SafeFlashSlotMode.OtherSlot, true)]
    [InlineData(SafeFlashSlotMode.BothSlots, true)]
    public void IsSlotBasedMode_is_false_only_for_current_slot(SafeFlashSlotMode mode, bool expected)
    {
        SafeFlashSlotPlanner.IsSlotBasedMode(mode).Should().Be(expected);
    }
}
