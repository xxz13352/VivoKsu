using System.ComponentModel;
using System.Diagnostics;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class VivoDriverInstallerTests
{
    [Fact]
    public void LocateBundle_returns_bundled_7z_path_when_present()
    {
        using var fixture = new TempRoot();
        var drivers = Path.Combine(fixture.Root, "drivers");
        Directory.CreateDirectory(drivers);
        File.WriteAllText(Path.Combine(drivers, VivoDriverInstaller.ArchiveFileName), "stub");

        var path = VivoDriverInstaller.LocateBundle(fixture.Root);

        Assert.NotNull(path);
        Assert.True(File.Exists(path));
        Assert.EndsWith(Path.Combine("drivers", VivoDriverInstaller.ArchiveFileName), path);
    }

    [Fact]
    public void LocateBundle_returns_null_when_bundle_missing()
    {
        using var fixture = new TempRoot();

        var path = VivoDriverInstaller.LocateBundle(fixture.Root);

        Assert.Null(path);
    }

    [Fact]
    public void Pnputil_arguments_use_wildcard_recursive_install_without_quiet()
    {
        // pnputil 真实语法为 /add-driver <filename.inf | *.inf> [/subdirs] [/install];
        // 多个显式 INF 路径与 /quiet 都会被整行拒绝。用 staging 通配符 + /subdirs 一次递归装全部。
        var arguments = VivoDriverInstaller.BuildPnputilArguments(@"C:\staging");

        Assert.Contains(@"/add-driver", arguments, StringComparison.Ordinal);
        Assert.Contains(@"C:\staging\*.inf", arguments, StringComparison.Ordinal);
        Assert.Contains("/subdirs", arguments, StringComparison.Ordinal);
        Assert.Contains("/install", arguments, StringComparison.Ordinal);
        Assert.DoesNotContain("/quiet", arguments, StringComparison.Ordinal);
    }

    [Fact]
    public async Task InstallAsync_extracts_runs_pnputil_and_writes_adb_usb_ini()
    {
        using var fixture = new TempRoot();
        var adbIni = Path.Combine(fixture.Root, "adb_usb.ini");
        ProcessStartInfo? captured = null;

        var installer = new VivoDriverInstaller(
            startAndWait: startInfo =>
            {
                captured = startInfo;
                return Task.FromResult(0);
            },
            archiveExtractor: async (_, destination) =>
            {
                Directory.CreateDirectory(destination);
                await File.WriteAllTextAsync(Path.Combine(destination, "androidwinusb.inf"), "stub");
                await File.WriteAllTextAsync(Path.Combine(destination, "cdc-acm.inf"), "stub");
            },
            adbUsbIniPath: adbIni);

        var exitCode = await installer.InstallAsync(@"C:\bundle.7z");

        Assert.Equal(0, exitCode);
        Assert.NotNull(captured);
        Assert.EndsWith("pnputil.exe", captured!.FileName);
        Assert.Equal("runas", captured.Verb);
        // 用 staging 通配符而非逐个 INF 路径,否则 pnputil 整行拒绝。
        Assert.Contains(@"\*.inf", captured.Arguments, StringComparison.Ordinal);
        Assert.Contains("/subdirs", captured.Arguments, StringComparison.Ordinal);
        Assert.Contains("/install", captured.Arguments, StringComparison.Ordinal);

        var lines = File.ReadAllLines(adbIni);
        Assert.Contains("0x2D95", lines);
        Assert.Contains("0x9BB5", lines);
        Assert.Contains("0x18D1", lines);
        Assert.Contains("0x0E8D", lines);
    }

    [Fact]
    public async Task InstallAsync_cleanup_removes_staging()
    {
        using var fixture = new TempRoot();
        string? capturedStaging = null;

        var installer = new VivoDriverInstaller(
            startAndWait: _ => Task.FromResult(0),
            archiveExtractor: async (_, destination) =>
            {
                capturedStaging = destination;
                Directory.CreateDirectory(destination);
                await File.WriteAllTextAsync(Path.Combine(destination, "x.inf"), "stub");
            },
            adbUsbIniPath: Path.Combine(fixture.Root, "adb_usb.ini"));

        await installer.InstallAsync(@"C:\bundle.7z");

        Assert.NotNull(capturedStaging);
        Assert.False(Directory.Exists(capturedStaging), "安装完成后应清理临时解压目录。");
    }

    [Fact]
    public async Task InstallAsync_nonzero_exit_skips_adb_usb_ini()
    {
        using var fixture = new TempRoot();
        var adbIni = Path.Combine(fixture.Root, "adb_usb.ini");

        var installer = new VivoDriverInstaller(
            startAndWait: _ => Task.FromResult(5),
            archiveExtractor: async (_, destination) =>
            {
                Directory.CreateDirectory(destination);
                await File.WriteAllTextAsync(Path.Combine(destination, "x.inf"), "stub");
            },
            adbUsbIniPath: adbIni);

        var exitCode = await installer.InstallAsync(@"C:\bundle.7z");

        Assert.Equal(5, exitCode);
        Assert.False(File.Exists(adbIni), "安装失败时不应写入 adb_usb.ini。");
    }

    [Fact]
    public async Task InstallAsync_no_inf_throws()
    {
        using var fixture = new TempRoot();
        var installer = new VivoDriverInstaller(
            startAndWait: _ => Task.FromResult(0),
            archiveExtractor: (_, _) => Task.CompletedTask,
            adbUsbIniPath: Path.Combine(fixture.Root, "adb_usb.ini"));

        await Assert.ThrowsAsync<InvalidOperationException>(() => installer.InstallAsync(@"C:\bundle.7z"));
    }

    [Fact]
    public async Task InstallAsync_uac_denied_propagates_cancelled()
    {
        using var fixture = new TempRoot();
        var installer = new VivoDriverInstaller(
            startAndWait: _ => throw new OperationCanceledException("已取消管理员授权,未安装驱动。"),
            archiveExtractor: async (_, destination) =>
            {
                Directory.CreateDirectory(destination);
                await File.WriteAllTextAsync(Path.Combine(destination, "x.inf"), "stub");
            },
            adbUsbIniPath: Path.Combine(fixture.Root, "adb_usb.ini"));

        await Assert.ThrowsAsync<OperationCanceledException>(() => installer.InstallAsync(@"C:\bundle.7z"));
    }

    private sealed class TempRoot : IDisposable
    {
        public TempRoot() => Root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));

        public string Root { get; }

        public void Dispose()
        {
            if (Directory.Exists(Root))
            {
                Directory.Delete(Root, true);
            }
        }
    }
}
