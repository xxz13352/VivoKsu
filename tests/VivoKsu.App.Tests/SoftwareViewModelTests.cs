using System.ComponentModel;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class SoftwareViewModelTests
{
    private static string MissingScrcpyRoot() =>
        Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", "no-scrcpy", Guid.NewGuid().ToString("N"));

    [Fact]
    public void AppVersion_comes_from_assembly_version()
    {
        var viewModel = CreateViewModel();

        // 形如 "1.0.0":三段点分隔,且每段是非负整数。
        Assert.Equal(2, viewModel.AppVersion.Count(c => c == '.'));
        Assert.All(viewModel.AppVersion.Split('.'), segment =>
            Assert.True(int.TryParse(segment, out _)));
    }

    [Fact]
    public async Task Driver_status_reflects_detector_across_three_categories()
    {
        using var fixture = new TempRoot();
        var notInstalled = new VivoDriverDetector(
            driverStoreDirectories: [Path.Combine(fixture.Root, "missing")],
            installDirectories: [Path.Combine(fixture.Root, "missing-install")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);
        var viewModel = CreateViewModel(driverDetector: notInstalled);
        await viewModel.RefreshCommand.ExecuteAsync(null);

        Assert.False(viewModel.IsAdbDriverInstalled);
        Assert.False(viewModel.IsFastbootDriverInstalled);
        Assert.False(viewModel.IsMediaTekDriverInstalled);
        Assert.Equal("未安装", viewModel.AdbDriverStatusText);
        Assert.Equal("未安装", viewModel.MediaTekDriverStatusText);

        // 只装 ADB 标记:仅 ADB 显示已安装,另两类未安装。
        Directory.CreateDirectory(Path.Combine(fixture.Root, "DriverStore", "FileRepository", "android_winusb.inf_amd64_x"));
        var onlyAdb = new VivoDriverDetector(
            driverStoreDirectories: [Path.Combine(fixture.Root, "DriverStore", "FileRepository")],
            installDirectories: [Path.Combine(fixture.Root, "missing-install")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);
        var onlyAdbViewModel = CreateViewModel(driverDetector: onlyAdb);
        await onlyAdbViewModel.RefreshCommand.ExecuteAsync(null);

        Assert.True(onlyAdbViewModel.IsAdbDriverInstalled);
        Assert.Equal("已安装", onlyAdbViewModel.AdbDriverStatusText);
        Assert.False(onlyAdbViewModel.IsFastbootDriverInstalled);
        Assert.False(onlyAdbViewModel.IsMediaTekDriverInstalled);
    }

    [Fact]
    public async Task Reinstall_driver_command_invokes_the_callback()
    {
        using var fixture = new TempRoot();
        var invoked = false;
        var detector = new VivoDriverDetector(
            driverStoreDirectories: [Path.Combine(fixture.Root, "missing")],
            installDirectories: [Path.Combine(fixture.Root, "missing-install")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);
        var viewModel = CreateViewModel(driverDetector: detector, onReinstallDriver: () => invoked = true);

        Assert.True(viewModel.ReinstallDriverCommand.CanExecute(null));
        viewModel.ReinstallDriverCommand.Execute(null);

        Assert.True(invoked);
    }

    [Fact]
    public async Task Scrcpy_status_reflects_locator()
    {
        var missing = new StubScrcpyLocator(isAvailable: false, status: "未检测到 scrcpy.exe");
        var viewModel = CreateViewModel(scrcpyLocator: missing, scrcpyInstallationRoot: MissingScrcpyRoot());
        await viewModel.RefreshCommand.ExecuteAsync(null);

        Assert.False(viewModel.IsScrcpyReady);
        Assert.Equal("未检测到 scrcpy.exe", viewModel.ScrcpyStatusText);

        var ready = new StubScrcpyLocator(isAvailable: true, status: "scrcpy 已就绪");
        var readyViewModel = CreateViewModel(scrcpyLocator: ready, scrcpyInstallationRoot: MissingScrcpyRoot());
        await readyViewModel.RefreshCommand.ExecuteAsync(null);

        Assert.True(readyViewModel.IsScrcpyReady);
        Assert.Equal("scrcpy 已就绪", readyViewModel.ScrcpyStatusText);
    }

    [Fact]
    public async Task Scrcpy_status_recognizes_user_selected_path_from_preferences()
    {
        using var fixture = new TempRoot();
        var settingsPath = Path.Combine(fixture.Root, "settings.json");
        Directory.CreateDirectory(Path.Combine(fixture.Root, "user-scrcpy"));
        var chosen = Path.Combine(fixture.Root, "user-scrcpy", "scrcpy.exe");
        File.WriteAllText(chosen, "stub");
        var prefs = new ToolPathPreferences(settingsPath);
        prefs.SaveScrcpyPath(chosen);

        var viewModel = CreateViewModel(
            scrcpyLocator: new StubScrcpyLocator(isAvailable: false, status: "未检测到"),
            preferences: prefs,
            scrcpyInstallationRoot: MissingScrcpyRoot());
        await viewModel.RefreshCommand.ExecuteAsync(null);

        Assert.True(viewModel.IsScrcpyReady);
    }

    [Fact]
    public async Task Payload_status_reflects_executable_presence()
    {
        using var fixture = new TempRoot();
        var missing = new PayloadDumperRunner(Path.Combine(fixture.Root, "missing", "payload_dumper.exe"));
        var viewModel = CreateViewModel(payloadDumper: missing);
        await viewModel.RefreshCommand.ExecuteAsync(null);

        Assert.False(viewModel.IsPayloadReady);
        Assert.Equal("未就绪", viewModel.PayloadStatusText);

        Directory.CreateDirectory(Path.Combine(fixture.Root, "payload-tools"));
        File.WriteAllText(Path.Combine(fixture.Root, "payload-tools", "payload_dumper.exe"), "stub");
        var ready = new PayloadDumperRunner(Path.Combine(fixture.Root, "payload-tools", "payload_dumper.exe"));
        var readyViewModel = CreateViewModel(payloadDumper: ready);
        await readyViewModel.RefreshCommand.ExecuteAsync(null);

        Assert.True(readyViewModel.IsPayloadReady);
        Assert.Equal("就绪", readyViewModel.PayloadStatusText);
    }

    [Fact]
    public async Task Refresh_command_notifies_property_changed_and_recomputes()
    {
        using var fixture = new TempRoot();
        var markerDirectory = Path.Combine(fixture.Root, "DriverStore", "FileRepository", "android_winusb.inf_amd64_x");
        var detector = new VivoDriverDetector(
            driverStoreDirectories: [Path.Combine(fixture.Root, "DriverStore", "FileRepository")],
            installDirectories: [Path.Combine(fixture.Root, "missing")],
            uninstallRegistryKeys: ["unused"],
            registryKeyExists: _ => false);
        var viewModel = CreateViewModel(driverDetector: detector);
        await viewModel.RefreshCommand.ExecuteAsync(null);
        Assert.False(viewModel.IsAdbDriverInstalled);

        // 订阅 PropertyChanged:Refresh 必须实际触发通知(而非仅 getter 惰性求值)。
        var notified = new List<string>();
        viewModel.PropertyChanged += (_, e) => notified.Add(e.PropertyName!);

        Directory.CreateDirectory(markerDirectory);
        await viewModel.RefreshCommand.ExecuteAsync(null);

        Assert.True(viewModel.IsAdbDriverInstalled);
        Assert.Contains(nameof(SoftwareViewModel.IsAdbDriverInstalled), notified);
        Assert.Contains(nameof(SoftwareViewModel.AdbDriverStatusText), notified);
    }

    private static SoftwareViewModel CreateViewModel(
        VivoDriverDetector? driverDetector = null,
        IScrcpyToolLocator? scrcpyLocator = null,
        PayloadDumperRunner? payloadDumper = null,
        ToolPathPreferences? preferences = null,
        string? scrcpyInstallationRoot = null,
        Action? onReinstallDriver = null) =>
        new(
            applicationRoot: Path.GetTempPath(),
            driverDetector,
            scrcpyLocator,
            payloadDumper,
            preferences,
            scrcpyInstallationRoot,
            onReinstallDriver);

    private sealed class StubScrcpyLocator : IScrcpyToolLocator
    {
        private readonly bool isAvailable;
        private readonly string status;

        public StubScrcpyLocator(bool isAvailable, string status)
        {
            this.isAvailable = isAvailable;
            this.status = status;
        }

        public bool IsAvailable => isAvailable;
        public string? ExecutablePath => isAvailable ? @"C:\scrcpy\scrcpy.exe" : null;
        public string StatusMessage => status;
        public void ConfigureToolPath(string toolPath) { }
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
