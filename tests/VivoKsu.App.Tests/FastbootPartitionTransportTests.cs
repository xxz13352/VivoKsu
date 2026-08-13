using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FastbootPartitionTransportTests
{
    [Fact]
    public async Task DiscoverAsync_returns_all_fastboot_partition_rows()
    {
        var native = new FastbootNativeApi
        {
            GetVarAll = "current-slot: b\npartition-size:boot_a:0x04000000\npartition-size:boot_b:0x04000000\npartition-size:super:0x200000000"
        };
        var transport = new FastbootPartitionTransport(new FastbootRsBackend(native));

        var snapshot = await transport.DiscoverAsync("FAST123", CancellationToken.None);

        snapshot.ActiveSlot.Should().Be("b");
        snapshot.Partitions.Should().HaveCount(3);
        snapshot.Partitions.Single(partition => partition.Name == "super").IsHighRisk.Should().BeTrue();
    }

    [Fact]
    public async Task WriteAsync_forwards_the_partition_and_selected_image()
    {
        var native = new FastbootNativeApi();
        var transport = new FastbootPartitionTransport(new FastbootRsBackend(native));
        var task = new PartitionTask("init_boot_a", "init_boot_a", @"D:\images\custom.bin", null, 8L * 1024 * 1024);

        await transport.WriteAsync("FAST123", task, progress: null, CancellationToken.None);

        native.FlashRequest.Should().NotBeNull();
        native.FlashRequest!.Value.Serial.Should().Be("FAST123");
        native.FlashRequest!.Value.Partition.Should().Be("init_boot_a");
        native.FlashRequest.Value.ImagePath.Should().Be(@"D:\images\custom.bin");
    }

    [Fact]
    public async Task EraseAsync_forwards_a_high_risk_partition_without_blocking_it()
    {
        var native = new FastbootNativeApi();
        var transport = new FastbootPartitionTransport(new FastbootRsBackend(native));
        var task = new PartitionTask("super", "super", null, null, 8L * 1024 * 1024 * 1024);

        await transport.EraseAsync("FAST123", task, progress: null, CancellationToken.None);

        native.EraseRequest.Should().NotBeNull();
        native.EraseRequest!.Value.Serial.Should().Be("FAST123");
        native.EraseRequest!.Value.Partition.Should().Be("super");
    }

    [Fact]
    public async Task BackupAsync_uses_the_selected_output_path()
    {
        var native = new FastbootNativeApi { FetchResult = 64 * 1024 * 1024 };
        var transport = new FastbootPartitionTransport(new FastbootRsBackend(native));
        var task = new PartitionTask("boot_a", "boot_a", null, @"D:\backups\boot_a.img", 64L * 1024 * 1024);

        await transport.BackupAsync("FAST123", task, progress: null, CancellationToken.None);

        native.FetchRequest.Should().NotBeNull();
        native.FetchRequest!.Value.Serial.Should().Be("FAST123");
        native.FetchRequest!.Value.Partition.Should().Be("boot_a");
        native.FetchRequest.Value.OutputPath.Should().Be(@"D:\backups\boot_a.img");
    }

    private sealed class FastbootNativeApi : IFastbootRsNativeApi
    {
        public string GetVarAll { get; init; } = string.Empty;
        public long FetchResult { get; init; }
        public (string? Serial, string Partition, string ImagePath)? FlashRequest { get; private set; }
        public (string? Serial, string Partition)? EraseRequest { get; private set; }
        public (string? Serial, string Partition, string OutputPath)? FetchRequest { get; private set; }

        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => variable == "all" ? GetVarAll : string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) => FlashRequest = (serial, partition, imagePath);
        public void Erase(string? serial, string partition) => EraseRequest = (serial, partition);
        public long Fetch(string? serial, string partition, string outputPath)
        {
            FetchRequest = (serial, partition, outputPath);
            return FetchResult;
        }
    }
}
