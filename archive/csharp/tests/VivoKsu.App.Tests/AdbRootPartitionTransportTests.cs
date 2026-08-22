using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class AdbRootPartitionTransportTests
{
    [Fact]
    public async Task DiscoverAsync_deduplicates_by_name_links_and_keeps_mounted_partitions()
    {
        var runner = new FakeAdbRootTransferRunner
        {
            RootResponses =
            [
                "0",
                "_b",
                "boot_b|/dev/block/sda13|67108864|0\nboot_b|/dev/block/sda13|67108864|0\nsuper|/dev/block/sda70|8589934592|1"
            ]
        };
        var transport = new AdbRootPartitionTransport(runner);

        var snapshot = await transport.DiscoverAsync("ADB123", CancellationToken.None);

        snapshot.Transport.Should().Be(PartitionTransportKind.AdbRoot);
        snapshot.ActiveSlot.Should().Be("b");
        snapshot.Partitions.Should().HaveCount(2);
        snapshot.Partitions.Single(partition => partition.Name == "super").IsMounted.Should().BeTrue();
        snapshot.Partitions.Single(partition => partition.Name == "super").IsHighRisk.Should().BeTrue();
    }

    [Fact]
    public async Task WriteAsync_streams_the_selected_image_to_the_resolved_block_path()
    {
        var runner = new FakeAdbRootTransferRunner { ResolvedPaths = { ["boot_a"] = "/dev/block/sda12" } };
        var transport = new AdbRootPartitionTransport(runner);
        var task = new PartitionTask("boot_a", "/dev/block/sda12", @"D:\images\custom.bin", null, 64L * 1024 * 1024);

        await transport.WriteAsync("ADB123", task, progress: null, CancellationToken.None);

        runner.WriteRequest.Should().NotBeNull();
        runner.WriteRequest!.Value.Serial.Should().Be("ADB123");
        runner.WriteRequest.Value.ImagePath.Should().Be(@"D:\images\custom.bin");
        runner.WriteRequest.Value.DevicePath.Should().Be("/dev/block/sda12");
    }

    [Fact]
    public async Task WriteAsync_refuses_to_target_a_partition_whose_path_changed_since_discovery()
    {
        var runner = new FakeAdbRootTransferRunner { ResolvedPaths = { ["boot_a"] = "/dev/block/sda99" } };
        var transport = new AdbRootPartitionTransport(runner);
        var task = new PartitionTask("boot_a", "/dev/block/sda12", @"D:\images\custom.bin", null, 64L * 1024 * 1024);

        var act = async () => await transport.WriteAsync("ADB123", task, progress: null, CancellationToken.None);

        await act.Should().ThrowAsync<PartitionOperationException>();
        runner.WriteRequest.Should().BeNull();
    }

    [Fact]
    public async Task EraseAsync_allows_a_high_risk_partition_and_forwards_its_resolved_path()
    {
        var runner = new FakeAdbRootTransferRunner { ResolvedPaths = { ["super"] = "/dev/block/sda70" } };
        var transport = new AdbRootPartitionTransport(runner);
        var task = new PartitionTask("super", "/dev/block/sda70", null, null, 8L * 1024 * 1024 * 1024);

        await transport.EraseAsync("ADB123", task, progress: null, CancellationToken.None);

        runner.EraseRequest.Should().NotBeNull();
        runner.EraseRequest!.Value.Serial.Should().Be("ADB123");
        runner.EraseRequest.Value.DevicePath.Should().Be("/dev/block/sda70");
    }

    [Fact]
    public async Task WriteAsync_resolves_partitions_on_devices_without_the_default_by_name_layout()
    {
        // 设备只有 /dev/block/bootdevice/by-name(无默认 /dev/block/by-name)。
        // 重解析必须遍历发现阶段接受的三种布局,否则此处 readlink 失败 → 操作无法执行。
        var runner = new FakeAdbRootTransferRunner { ResolvedPaths = { ["boot_a"] = "/dev/block/sda12" } };
        var transport = new AdbRootPartitionTransport(runner);
        var task = new PartitionTask("boot_a", "/dev/block/sda12", @"D:\images\custom.bin", null, 64L * 1024 * 1024);

        await transport.WriteAsync("ADB123", task, progress: null, CancellationToken.None);

        runner.WriteRequest.Should().NotBeNull();
        runner.WriteRequest!.Value.DevicePath.Should().Be("/dev/block/sda12");
    }

    private sealed class FakeAdbRootTransferRunner : IAdbRootTransferRunner
    {
        private int rootResponseIndex;

        public IReadOnlyList<string> RootResponses { get; init; } = [];
        public Dictionary<string, string> ResolvedPaths { get; } = [];
        public (string Serial, string ImagePath, string DevicePath)? WriteRequest { get; private set; }
        public (string Serial, string DevicePath)? EraseRequest { get; private set; }

        public Task<string> RunRootAsync(string serial, string command, CancellationToken cancellationToken)
        {
            // 多布局重解析命令:for d in ...; do [ -e "$d/{name}" ] ... done。
            // 用 [ -e "$d/ 特征区分(发现命令是 [ -d "$d" ])。
            if (command.Contains("[ -e \"$d/", StringComparison.Ordinal))
            {
                var name = ExtractResolvedName(command);
                return Task.FromResult(ResolvedPaths.TryGetValue(name, out var path) ? path : string.Empty);
            }

            var response = rootResponseIndex < RootResponses.Count ? RootResponses[rootResponseIndex] : string.Empty;
            rootResponseIndex++;
            return Task.FromResult(response);
        }

        private static string ExtractResolvedName(string command)
        {
            const string marker = "[ -e \"$d/";
            var start = command.IndexOf(marker, StringComparison.Ordinal);
            if (start < 0)
            {
                return string.Empty;
            }

            start += marker.Length;
            var end = command.IndexOf("\" ]", start, StringComparison.Ordinal);
            return end < 0 ? string.Empty : command[start..end].Trim();
        }

        public Task CopyFromDeviceAsync(string serial, string devicePath, string localPath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) =>
            Task.CompletedTask;

        public Task CopyToDeviceAsync(string serial, string localImagePath, string devicePath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
        {
            WriteRequest = (serial, localImagePath, devicePath);
            return Task.CompletedTask;
        }

        public Task EraseAsync(string serial, string devicePath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
        {
            EraseRequest = (serial, devicePath);
            return Task.CompletedTask;
        }
    }
}
