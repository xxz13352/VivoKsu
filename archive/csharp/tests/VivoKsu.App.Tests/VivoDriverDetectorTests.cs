using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class VivoDriverDetectorTests
{
    [Fact]
    public void Not_installed_when_driver_store_empty_and_no_install_dir_or_registry()
    {
        using var fixture = new Fixture();

        var detector = CreateDetector(fixture, registryKeyExists: _ => false);

        Assert.False(detector.IsAnyInstalled);
        Assert.False(detector.IsAllInstalled);
        Assert.False(detector.IsAdbInstalled);
        Assert.False(detector.IsFastbootInstalled);
        Assert.False(detector.IsMediaTekInstalled);
    }

    [Fact]
    public void Adb_marker_sets_adb_installed_only()
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, "android_winusb.inf_amd64_abc123"));
        var detector = CreateDetector(fixture);

        Assert.True(detector.IsAdbInstalled);
        Assert.False(detector.IsFastbootInstalled);
        Assert.False(detector.IsMediaTekInstalled);
        Assert.False(detector.IsAllInstalled);
    }

    [Fact]
    public void Fastboot_marker_sets_fastboot_installed_only()
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, "android_usb.inf_amd64_abc123"));
        var detector = CreateDetector(fixture);

        Assert.False(detector.IsAdbInstalled);
        Assert.True(detector.IsFastbootInstalled);
        Assert.False(detector.IsMediaTekInstalled);
        Assert.False(detector.IsAllInstalled);
    }

    [Fact]
    public void MediaTek_driver_with_mediatek_inf_sets_mediaTek_installed()
    {
        using var fixture = new Fixture();
        var folder = Path.Combine(fixture.DriverStore, "cdc-acm.inf_amd64_abc123");
        Directory.CreateDirectory(folder);
        File.WriteAllText(Path.Combine(folder, "cdc-acm.inf"), "Provider=MediaTek Inc.\r\n");
        var detector = CreateDetector(fixture);

        Assert.False(detector.IsAdbInstalled);
        Assert.False(detector.IsFastbootInstalled);
        Assert.True(detector.IsMediaTekInstalled);
        Assert.False(detector.IsAllInstalled);
    }

    [Theory]
    [InlineData("ftdibus.inf_amd64_abc123")]
    [InlineData("ftdiport.inf_amd64_abc123")]
    [InlineData("cdc-acm.inf_amd64_abc123")]   // cdc-acm 目录但 INF 非 MediaTek(CH340/其它 CDC 串口)
    public void Generic_serial_drivers_do_not_count_as_mediaTek(string folderName)
    {
        // 任意 FTDI/CDC 设备都会 staging ftdibus/ftdiport/cdc-acm;无 "MediaTek" 内容不算联发科驱动。
        using var fixture = new Fixture();
        var folder = Path.Combine(fixture.DriverStore, folderName);
        Directory.CreateDirectory(folder);
        File.WriteAllText(Path.Combine(folder, Path.GetFileName(folderName)), "Provider=Generic USB Serial\r\n");
        var detector = CreateDetector(fixture);

        Assert.False(detector.IsMediaTekInstalled);
    }

    [Fact]
    public void All_three_markers_set_all_installed()
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, "android_winusb.inf_amd64_a"));
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, "android_usb.inf_amd64_b"));
        var mtkFolder = Path.Combine(fixture.DriverStore, "cdc-acm.inf_amd64_c");
        Directory.CreateDirectory(mtkFolder);
        File.WriteAllText(Path.Combine(mtkFolder, "cdc-acm.inf"), "Provider=MediaTek Inc.\r\n");
        var detector = CreateDetector(fixture);

        Assert.True(detector.IsAllInstalled);
        Assert.True(detector.IsInstalled());
    }

    [Fact]
    public void Marker_match_is_case_insensitive()
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, "ANDROID_WINUSB.INF_AMD64_abc123"));
        var detector = CreateDetector(fixture);

        Assert.True(detector.IsAdbInstalled);
    }

    [Theory]
    [InlineData("mdmcpq.inf_amd64_abc123")]
    [InlineData("usbprint.inf_amd64_abc123")]
    public void Unrelated_infs_do_not_count_as_driver(string folderName)
    {
        using var fixture = new Fixture();
        Directory.CreateDirectory(Path.Combine(fixture.DriverStore, folderName));
        var detector = CreateDetector(fixture);

        Assert.False(detector.IsAdbInstalled);
        Assert.False(detector.IsFastbootInstalled);
        Assert.False(detector.IsMediaTekInstalled);
    }

    [Fact]
    public void Legacy_bbk_install_directory_with_inf_marks_all_three_installed()
    {
        using var fixture = new Fixture();
        var legacyDir = Path.Combine(fixture.Root, "BBK", "vivo_usb_driver");
        Directory.CreateDirectory(legacyDir);
        File.WriteAllText(Path.Combine(legacyDir, "vivo.inf"), "Provider=vivo, Inc.\r\n");
        var detector = CreateDetector(fixture, installDir: legacyDir);

        Assert.True(detector.IsAllInstalled);
    }

    [Fact]
    public void Residual_empty_bbk_directory_does_not_count_as_installed()
    {
        // 卸载残留的空目录不应让三类误报已装。
        using var fixture = new Fixture();
        var legacyDir = Path.Combine(fixture.Root, "BBK", "vivo_usb_driver");
        Directory.CreateDirectory(legacyDir);
        var detector = CreateDetector(fixture, installDir: legacyDir);

        Assert.False(detector.IsAnyInstalled);
    }

    [Fact]
    public void Uninstall_registry_key_marks_all_three_installed()
    {
        using var fixture = new Fixture();
        var expectedKey = @"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\vivo_usb_driver_is1";
        string? seenKey = null;
        var detector = new VivoDriverDetector(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: [expectedKey],
            registryKeyExists: key => { seenKey = key; return true; });

        Assert.True(detector.IsAllInstalled);
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

        Assert.False(detector.IsAnyInstalled);
    }

    private static VivoDriverDetector CreateDetector(Fixture fixture, string? installDir = null, Func<string, bool>? registryKeyExists = null) =>
        new(
            driverStoreDirectories: [fixture.DriverStore],
            installDirectories: [installDir ?? Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: registryKeyExists ?? (_ => false));

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
