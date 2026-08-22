using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FastbootPartitionTransportTests
{
    [Fact]
    public async Task DiscoverAsync_returns_all_fastboot_partition_rows()
    {
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable == "all"
                ? "current-slot: b\npartition-size:boot_a:0x04000000\npartition-size:boot_b:0x04000000\npartition-size:super:0x200000000"
                : string.Empty
        };
        var transport = new FastbootPartitionTransport(fake);

        var snapshot = await transport.DiscoverAsync("FAST123", CancellationToken.None);

        snapshot.ActiveSlot.Should().Be("b");
        snapshot.Partitions.Should().HaveCount(3);
        snapshot.Partitions.Single(partition => partition.Name == "super").IsHighRisk.Should().BeTrue();
    }

    [Fact]
    public async Task WriteAsync_forwards_the_partition_and_selected_image()
    {
        var fake = new FakeFastbootCliRunner();
        var transport = new FastbootPartitionTransport(fake);
        var task = new PartitionTask("init_boot_a", "init_boot_a", @"D:\images\custom.bin", null, 8L * 1024 * 1024);

        await transport.WriteAsync("FAST123", task, progress: null, CancellationToken.None);

        fake.LastFlash.Should().NotBeNull();
        fake.LastFlash!.Value.Partition.Should().Be("init_boot_a");
        fake.LastFlash!.Value.ImagePath.Should().Be(@"D:\images\custom.bin");
    }

    [Fact]
    public async Task EraseAsync_forwards_a_high_risk_partition_without_blocking_it()
    {
        var fake = new FakeFastbootCliRunner();
        var transport = new FastbootPartitionTransport(fake);
        var task = new PartitionTask("super", "super", null, null, 8L * 1024 * 1024 * 1024);

        await transport.EraseAsync("FAST123", task, progress: null, CancellationToken.None);

        fake.Erased.Should().Contain("super");
    }

    [Fact]
    public async Task BackupAsync_reports_fastboot_mode_does_not_support_readback()
    {
        var fake = new FakeFastbootCliRunner();
        var transport = new FastbootPartitionTransport(fake);
        var task = new PartitionTask("boot_a", "boot_a", null, @"D:\backups\boot_a.img", 64L * 1024 * 1024);

        var act = async () => await transport.BackupAsync("FAST123", task, progress: null, CancellationToken.None);

        await act.Should().ThrowAsync<PartitionOperationException>();
    }
}
