using System.Collections;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FastbootPartitionServiceTests
{
    [Fact]
    public async Task ReadAsync_formats_supported_partition_sizes_and_fastboot_metadata()
    {
        var backend = new FastbootRsBackend(new PartitionNativeApi(new Dictionary<string, string>
        {
            ["current-slot"] = "a",
            ["is-userspace"] = "no",
            ["partition-size:boot"] = "0x04000000",
            ["partition-size:init_boot"] = "0x00800000",
            ["partition-size:vendor_boot"] = "0x06000000"
        }));

        var snapshot = await ReadTableAsync(backend);

        Assert.Equal("a", Get(snapshot, "ActiveSlot"));
        Assert.Equal("fastboot", Get(snapshot, "ModeLabel"));

        var boot = FindPartition(snapshot, "boot");
        Assert.Equal("64 MB", Get(boot, "SizeDisplay"));
        Assert.Equal("已读取", Get(boot, "Status"));

        var vendorBoot = FindPartition(snapshot, "vendor_boot");
        Assert.Equal("96 MB", Get(vendorBoot, "SizeDisplay"));
    }

    [Fact]
    public async Task ReadAsync_keeps_an_unsupported_partition_as_unavailable()
    {
        var backend = new FastbootRsBackend(new PartitionNativeApi(new Dictionary<string, string>
        {
            ["current-slot"] = "b",
            ["is-userspace"] = "yes",
            ["partition-size:boot"] = "0x04000000",
            ["partition-size:init_boot"] = "0x00800000"
        }));

        var snapshot = await ReadTableAsync(backend);
        var vendorBoot = FindPartition(snapshot, "vendor_boot");

        Assert.Equal("fastbootd", Get(snapshot, "ModeLabel"));
        Assert.Equal("--", Get(vendorBoot, "SizeDisplay"));
        Assert.Equal("未读取", Get(vendorBoot, "Status"));
    }

    private static async Task<object> ReadTableAsync(FastbootRsBackend backend)
    {
        var assembly = typeof(FastbootRsBackend).Assembly;
        var serviceType = assembly.GetType("VivoKsu.App.Services.FastbootPartitionService");
        Assert.NotNull(serviceType);

        var service = Activator.CreateInstance(serviceType!, backend);
        Assert.NotNull(service);

        var method = serviceType.GetMethod("ReadAsync");
        Assert.NotNull(method);

        var task = method!.Invoke(service, ["FAST123", CancellationToken.None]) as Task;
        Assert.NotNull(task);
        await task!;

        return task!.GetType().GetProperty("Result")!.GetValue(task)!;
    }

    private static object FindPartition(object snapshot, string name)
    {
        var partitions = (IEnumerable)Get(snapshot, "Partitions");
        return partitions.Cast<object>().Single(partition => (string)Get(partition, "Name") == name);
    }

    private static object Get(object value, string property) =>
        value.GetType().GetProperty(property)!.GetValue(value)!;

    private sealed class PartitionNativeApi(IReadOnlyDictionary<string, string> values) : IFastbootRsNativeApi
    {
        public string ListDevices() => "FAST123\tfastboot\n";
        public string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => values.TryGetValue(variable, out var value) ? value : string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
