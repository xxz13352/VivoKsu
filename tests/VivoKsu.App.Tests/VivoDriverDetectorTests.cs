using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class VivoDriverDetectorTests
{
    [Fact]
    public void Not_installed_when_driver_store_empty_and_no_install_dir_or_registry()
    {
        using var fixture = new Fixture();

        var detector = new VivoDriverDetector(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: [@"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\vivo_usb_driver_is1"],
            registryKeyExists: _ => false);

        Assert.False(detector.IsInstalled());
    }

    [Theory]
    [InlineData("androidwinusb.inf_amd64_abc123")]
    [InlineData("android_usb.inf_amd64_abc123")]
    public void Installed_when_driver_store_has_any_key_inf_marker(string folderName)
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, folderName));

        var detector = new VivoDriverDetector(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);

        Assert.True(detector.IsInstalled());
    }

    [Fact]
    public void Marker_match_is_case_insensitive()
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, "ANDROIDWINUSB.INF_AMD64_abc123"));

        var detector = new VivoDriverDetector(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);

        Assert.True(detector.IsInstalled());
    }

    [Theory]
    [InlineData("mdmcpq.inf_amd64_abc123")]
    [InlineData("usbprint.inf_amd64_abc123")]
    [InlineData("cdc-acm.inf_amd64_abc123")]      // 通用 CDC 串口(MediaTek/其它设备都 staging)
    [InlineData("ftdibus.inf_amd64_abc123")]      // 通用 FTDI 串口(Arduino/USB-TTL 等)
    [InlineData("ftdiport.inf_amd64_abc123")]     // 通用 FTDI 串口
    [InlineData("android_winusb.inf_amd64_abc123")] // Google ADB(带下划线,与本驱动包命名不同)
    public void Generic_or_unrelated_infs_do_not_count_as_vivo_driver(string folderName)
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, folderName));

        var detector = new VivoDriverDetector(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);

        Assert.False(detector.IsInstalled());
    }

    [Fact]
    public void Installed_when_bbk_install_directory_exists()
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.Root, "BBK", "vivo_usb_driver"));

        var detector = new VivoDriverDetector(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [Path.Combine(fixture.Root, "BBK", "vivo_usb_driver")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);

        Assert.True(detector.IsInstalled());
    }

    [Fact]
    public void Installed_when_uninstall_registry_key_exists()
    {
        using var fixture = new Fixture();
        var expectedKey = @"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\vivo_usb_driver_is1";
        string? seenKey = null;

        var detector = new VivoDriverDetector(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: [expectedKey],
            registryKeyExists: key => { seenKey = key; return true; });

        Assert.True(detector.IsInstalled());
        Assert.Equal(expectedKey, seenKey);
    }

    [Fact]
    public void Missing_driver_store_directory_is_treated_as_not_installed()
    {
        using var fixture = new Fixture();
        var missingStore = Path.Combine(fixture.Root, "no-such-driverstore");

        var detector = new VivoDriverDetector(
            driverStoreDirectories: [missingStore],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);

        Assert.False(detector.IsInstalled());
    }

    private sealed class Fixture : IDisposable
    {
        public Fixture()
        {
            Root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
            DriverStore = Path.Combine(Root, "DriverStore", "FileRepository");
            Directory.CreateDirectory(DriverStore);
        }

        public string Root { get; }

        public string DriverStore { get; }

        public void Dispose()
        {
            if (Directory.Exists(Root))
            {
                Directory.Delete(Root, true);
            }
        }
    }
}
