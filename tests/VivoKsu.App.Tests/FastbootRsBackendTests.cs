using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FastbootRsBackendTests
{
    [Fact]
    public async Task DiscoverAsync_uses_the_native_device_listing()
    {
        var backend = new FastbootRsBackend(new FakeNativeApi("A1B2C3\tfastboot\n"));

        var device = await backend.DiscoverAsync(CancellationToken.None);

        Assert.Equal(DeviceConnectionState.FastbootConnected, device.ConnectionState);
        Assert.Equal("A1B2C3", device.Serial);
    }

    [Fact]
    public async Task ShellAsync_forwards_the_serial_and_command()
    {
        var api = new FakeNativeApi("A1B2\tdevice\n")
        {
            ShellResult = "ro.product.model=iQOO 12"
        };
        var backend = new FastbootRsBackend(api);

        var output = await backend.ShellAsync("A1B2", "getprop ro.product.model", CancellationToken.None);

        Assert.Equal("ro.product.model=iQOO 12", output);
        Assert.Equal(("A1B2", "getprop ro.product.model"), api.LastShellRequest);
    }

    [Fact]
    public async Task SetActiveAsync_forwards_serial_and_slot_to_native_api()
    {
        var native = new FakeNativeApi(string.Empty);
        var backend = new FastbootRsBackend(native);

        await backend.SetActiveAsync("FAST456", "b", CancellationToken.None);

        Assert.Equal(("FAST456", "b"), native.SetActiveRequest);
    }

    [Fact]
    public async Task EraseAsync_forwards_the_selected_partition_to_native_api()
    {
        var native = new FakeNativeApi(string.Empty);
        var backend = new FastbootRsBackend(native);

        await backend.EraseAsync("FAST456", "super", CancellationToken.None);

        Assert.Equal(("FAST456", "super"), native.EraseRequest);
    }

    [Fact]
    public async Task FetchAsync_returns_the_native_backup_size()
    {
        var native = new FakeNativeApi(string.Empty) { FetchResult = 6_291_456 };
        var backend = new FastbootRsBackend(native);

        var bytes = await backend.FetchAsync("FAST456", "boot_a", @"D:\backups\boot_a.img", CancellationToken.None);

        Assert.Equal(6_291_456, bytes);
        Assert.Equal(("FAST456", "boot_a", @"D:\backups\boot_a.img"), native.FetchRequest);
    }

    private sealed class FakeNativeApi(string devices) : IFastbootRsNativeApi
    {
        public string ShellResult { get; set; } = string.Empty;
        public long FetchResult { get; set; }
        public (string? Serial, string Command)? LastShellRequest { get; private set; }
        public (string? Serial, string Slot)? SetActiveRequest { get; private set; }
        public (string? Serial, string Partition)? EraseRequest { get; private set; }
        public (string? Serial, string Partition, string OutputPath)? FetchRequest { get; private set; }

        public string ListDevices() => devices;
        public string Shell(string? serial, string command)
        {
            LastShellRequest = (serial, command);
            return ShellResult;
        }

        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => "Success";
        public void Flash(string? serial, string partition, string imagePath) { }
        public void SetActive(string? serial, string slot) => SetActiveRequest = (serial, slot);
        public void Erase(string? serial, string partition) => EraseRequest = (serial, partition);
        public long Fetch(string? serial, string partition, string outputPath)
        {
            FetchRequest = (serial, partition, outputPath);
            return FetchResult;
        }
    }
}
