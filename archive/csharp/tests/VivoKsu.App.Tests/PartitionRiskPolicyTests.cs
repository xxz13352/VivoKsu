using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class PartitionRiskPolicyTests
{
    [Theory]
    [InlineData("abl")]
    [InlineData("frp")]
    [InlineData("gpt")]
    [InlineData("lk")]
    [InlineData("metadata")]
    [InlineData("modemst")]
    [InlineData("modemst1")]
    [InlineData("modemst2")]
    [InlineData("modemst_a")]
    [InlineData("partition")]
    [InlineData("persist")]
    [InlineData("preloader")]
    [InlineData("super")]
    [InlineData("userdata")]
    [InlineData("vbmeta")]
    [InlineData("xbl")]
    [InlineData("xbl_1")]
    public void IsHighRisk_flags_critical_bootloader_and_modem_partitions(string partitionName)
    {
        Assert.True(PartitionRiskPolicy.IsHighRisk(partitionName));
    }

    [Theory]
    [InlineData("boot")]
    [InlineData("init_boot")]
    [InlineData("vendor_boot")]
    [InlineData("system")]
    [InlineData("cache")]
    [InlineData("recovery")]
    public void IsHighRisk_does_not_flag_ordinary_partitions(string partitionName)
    {
        Assert.False(PartitionRiskPolicy.IsHighRisk(partitionName));
    }
}
