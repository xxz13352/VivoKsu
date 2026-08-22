using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class DeviceInfoServiceTests
{
    [Fact]
    public async Task ReadAdbAsync_maps_getprop_output_into_device_details()
    {
        const string properties = "[ro.product.brand]: [vivo]\n[ro.product.model]: [V2318A]\n[ro.product.device]: [PD2307]\n[ro.build.version.release]: [15]\n[ro.build.display.id]: [PD2307_A_15.0.12.1.W10]\n[ro.boot.flash.locked]: [0]\n";
        var service = new DeviceInfoService(new FastbootRsBackend(new PropertyNativeApi(properties)), new FakeFastbootCliRunner());

        var details = await service.ReadAdbAsync("RF8", CancellationToken.None);

        Assert.Equal("vivo", details.Brand);
        Assert.Equal("V2318A", details.Model);
        Assert.Equal("PD2307", details.Codename);
        Assert.Equal("15", details.AndroidVersion);
        Assert.Equal("PD2307_A_15.0.12.1.W10", details.FirmwareVersion);
        Assert.Equal("unlocked", details.BootloaderState);
    }

    [Fact]
    public async Task ReadFastbootAsync_uses_product_when_prior_details_are_unavailable()
    {
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable switch
            {
                "current-slot" => "b",
                "unlocked" => "yes",
                "product" => "V2318A",
                _ => string.Empty
            }
        };
        var service = new DeviceInfoService(new FastbootRsBackend(new EmptyNativeApi()), fake);

        var details = await service.ReadFastbootAsync(
            DeviceDetailsSnapshot.Empty with { Serial = "FAST123" },
            CancellationToken.None);

        Assert.Equal("V2318A", details.Model);
        Assert.Equal("V2318A", details.Codename);
        Assert.Equal("b", details.ActiveSlot);
        Assert.Equal("unlocked", details.BootloaderState);
    }

    private sealed class PropertyNativeApi(string properties) : IFastbootRsNativeApi
    {
        public string ListDevices() => "RF8\tdevice\n";
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => command == "getprop" ? properties : string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath, int timeoutMilliseconds = 15000) { }
        public long Pull(string? serial, string remotePath, string localPath, int timeoutMilliseconds = 15000) => 0;
        public string Install(string? serial, string apkPath, bool replace) => "Success";
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class EmptyNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => "FAST123\tfastboot\n";
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath, int timeoutMilliseconds = 15000) { }
        public long Pull(string? serial, string remotePath, string localPath, int timeoutMilliseconds = 15000) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
