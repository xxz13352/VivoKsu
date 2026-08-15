using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public sealed class RootViewModelTests
{
    [Fact]
    public void Vivo_root_exposes_two_mutually_exclusive_manager_options()
    {
        var root = CreateRoot(new DeviceSessionViewModel(), new EmptyNativeApi());

        Assert.True(root.IsVivoKsuSelected);
        Assert.False(root.IsOfficialKsuSelected);
        Assert.Equal(["Vivo KSU", "官方 KernelSU"], root.ManagerOptions);

        root.IsOfficialKsuSelected = true;

        Assert.False(root.IsVivoKsuSelected);
        Assert.True(root.IsOfficialKsuSelected);
        Assert.Equal("OfficialKsu", root.SelectedManagerKey);
    }

    [Fact]
    public void Official_KernelSU_allows_partial_patch_but_requires_both_for_automatic_flow()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123", KernelVersion = "6.1.75" });
        var root = CreateRoot(session, new EmptyNativeApi());
        root.IsOfficialKsuSelected = true;
        root.SelectedImage = new FlashImageInfo("C:\\images\\init_boot.img", 1024);

        Assert.True(root.IsReadyForPatch);
        Assert.True(root.PatchImageCommand.CanExecute(null));
        Assert.False(root.RunAutomaticRootCommand.CanExecute(null));
        Assert.Contains("全自动需要两份镜像", root.PreflightSummary, StringComparison.Ordinal);

        root.SelectedVendorImage = new FlashImageInfo("C:\\images\\vendor_boot.img", 2048);

        Assert.True(root.IsReadyForPatch);
        Assert.True(root.RunAutomaticRootCommand.CanExecute(null));
        Assert.Equal("官方 KernelSU", root.SelectedManagerLabel);
    }

    [Fact]
    public void Official_KernelSU_can_patch_vendor_boot_without_init_boot()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123", KernelVersion = "6.1.75" });
        var root = CreateRoot(session, new EmptyNativeApi());
        root.IsOfficialKsuSelected = true;
        root.SelectedVendorImage = new FlashImageInfo("C:\\images\\payload.bin", 2048);

        Assert.True(root.IsReadyForPatch);
        Assert.True(root.PatchImageCommand.CanExecute(null));
        Assert.False(root.RunAutomaticRootCommand.CanExecute(null));
    }

    [Fact]
    public void Vivo_KSU_only_requires_init_boot()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123", KernelVersion = "6.1.75" });
        var root = CreateRoot(session, new EmptyNativeApi());
        root.SelectedImage = new FlashImageInfo("C:\\images\\init_boot.img", 1024);

        Assert.True(root.IsReadyForPatch);
        Assert.False(root.IsOfficialKsuSelected);
    }

    [Fact]
    public void Root_images_only_require_img_or_bin_extensions_not_partition_names()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123", KernelVersion = "6.1.75" });
        var root = CreateRoot(session, new EmptyNativeApi());
        root.SelectedImage = new FlashImageInfo("C:\\images\\payload-from-oem.bin", 1024);

        Assert.True(root.IsReadyForPatch);

        root.IsOfficialKsuSelected = true;
        root.SelectedVendorImage = new FlashImageInfo("C:\\images\\ramdisk-release.img", 2048);

        Assert.True(root.IsReadyForPatch);
    }

    [Fact]
    public void Automatic_KMI_is_enabled_by_default_and_uses_the_connected_device_kernel()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123", KernelVersion = "6.1.75" });
        var root = CreateRoot(session, new EmptyNativeApi());

        Assert.True(root.UseAutomaticKmi);
        Assert.Equal("android14-6.1", root.EffectiveKmi);
    }

    [Fact]
    public void Manual_KMI_override_uses_the_selected_supported_value()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123", KernelVersion = "6.1.75" });
        var root = CreateRoot(session, new EmptyNativeApi());
        root.UseAutomaticKmi = false;
        root.SelectedKmi = "android15-6.6";

        Assert.Equal("android15-6.6", root.EffectiveKmi);
    }

    [Fact]
    public async Task Installing_the_selected_manager_verifies_and_launches_its_activity()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123" });
        var native = new RecordingNativeApi();
        var root = CreateRoot(session, native);

        await root.InstallManagerCommand.ExecuteAsync(null);

        Assert.Contains("pm path me.inkdye.vivoksu", native.ShellCommands);
        Assert.Contains("am start -n me.inkdye.vivoksu/me.inkdye.vivoksu.ui.MainActivity", native.ShellCommands);
        Assert.Equal("Vivo KSU 管理器已安装并启动", session.StatusText);
    }

    [Fact]
    public async Task Automatic_root_flow_uses_one_shared_coordinator_lifecycle()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var imagePath = Path.Combine(directory, "init_boot.img");
        await File.WriteAllBytesAsync(imagePath, "stock-image"u8.ToArray());
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB123", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Serial = "ADB123", KernelVersion = "6.1.75" });
        var logs = new OperationLogService();
        var native = new AutomaticRootNativeApi();
        var backend = new FastbootRsBackend(native);
        var coordinator = new OperationCoordinator(session, logs);
        var fake = new FakeFastbootCliRunner
        {
            // VIVO root 走 fastbootd:等待目标匹配读的是 cliRunner.getvar is-userspace,
            // 必须是 "yes"(userspace fastboot)设备才能刷写。
            GetVarHandler = variable => variable == "is-userspace" ? "yes" : string.Empty
        };
        var artifacts = new RootPatchArtifactService(() => directory);
        var root = new RootViewModel(
            session,
            new QuickFlashService(backend, fake, logs),
            logs,
            backend,
            new VivoRootResourceService(AppContext.BaseDirectory),
            coordinator,
            artifacts)
        {
            SelectedImage = new FlashImageInfo(imagePath, new FileInfo(imagePath).Length)
        };

        try
        {
            await root.RunAutomaticRootCommand.ExecuteAsync(null);

            Assert.Contains(("ADB123", "fastboot"), native.Reboots);
            Assert.Contains(
                ("ADB123", "init_boot", Path.Combine(directory, RootPatchArtifactService.OutputFolderName, "init_boot_vivoksu_patched.img")),
                fake.FlashRequests);
            Assert.False(coordinator.IsBusy);
            Assert.True(
                session.OperationKind == OperationKind.Completed,
                string.Join(Environment.NewLine, logs.Entries.Select(entry => $"{entry.Level}: {entry.Message}")));
            Assert.Contains(logs.Entries, entry => entry.OperationId is not null && entry.Level == OperationLogLevel.Success);
        }
        finally
        {
            Directory.Delete(directory, true);
        }
    }

    private static RootViewModel CreateRoot(DeviceSessionViewModel session, IFastbootRsNativeApi native)
    {
        var logs = new OperationLogService();
        var backend = new FastbootRsBackend(native);
        return new RootViewModel(
            session,
            new QuickFlashService(backend, new FakeFastbootCliRunner(), logs),
            logs,
            backend,
            new VivoRootResourceService(AppContext.BaseDirectory));
    }

    private class EmptyNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public virtual string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => command.StartsWith("pm path", StringComparison.Ordinal) ? "package:/data/app/manager.apk" : string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => "Success";
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class RecordingNativeApi : EmptyNativeApi
    {
        public List<string> ShellCommands { get; } = [];

        public override string Shell(string? serial, string command, int timeoutMilliseconds = 15000)
        {
            ShellCommands.Add(command);
            return base.Shell(serial, command);
        }
    }

    private sealed class AutomaticRootNativeApi : IFastbootRsNativeApi
    {
        private bool isFastboot;

        public List<(string Serial, string Target)> Reboots { get; } = [];

        public List<(string Serial, string Partition, string ImagePath)> Flashes { get; } = [];

        public string ListDevices() => isFastboot ? "ADB123\tfastboot\n" : "ADB123\tdevice\n";

        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => command.StartsWith("pm path", StringComparison.Ordinal)
            ? "package:/data/app/manager.apk"
            : string.Empty;

        public string GetVar(string? serial, string variable) => variable == "is-userspace" ? "no" : string.Empty;

        public void Reboot(string? serial, string target)
        {
            Reboots.Add((serial ?? string.Empty, target));
            if (target == "fastboot")
            {
                isFastboot = true;
            }
        }

        public void FastbootReboot(string? serial, string? target) { }

        public void Push(string? serial, string localPath, string remotePath) { }

        public long Pull(string? serial, string remotePath, string localPath)
        {
            var payload = "patched-image"u8.ToArray();
            File.WriteAllBytes(localPath, payload);
            return payload.Length;
        }

        public string Install(string? serial, string apkPath, bool replace) => "Success";

        public void Flash(string? serial, string partition, string imagePath) =>
            Flashes.Add((serial ?? string.Empty, partition, imagePath));
    }
}
