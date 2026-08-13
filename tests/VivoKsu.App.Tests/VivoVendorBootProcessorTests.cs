using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public sealed class VivoVendorBootProcessorTests
{
    [Fact]
    public async Task Patches_both_official_and_gki_kernel_module_paths_and_repackages()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var sourcePath = Path.Combine(directory, "vendor_boot.img");
        await File.WriteAllBytesAsync(sourcePath, "vendor-stock"u8.ToArray());
        var source = await InspectAsync(sourcePath);
        var native = new VendorPatchNativeApi();
        var backend = new FastbootRsBackend(native);
        var service = new VivoVendorBootProcessor(backend, new VivoRootResourceService(AppContext.BaseDirectory), new QuickFlashService(backend, new FakeFastbootCliRunner(), new OperationLogService()));

        try
        {
            var result = await service.PatchAsync("ADB123", source, CancellationToken.None);

            Assert.True(File.Exists(result.Path));
            Assert.Equal("vendor_boot_vivo_patched.img", Path.GetFileName(result.Path));
            Assert.Contains(native.PushedRemotePaths, path => path.EndsWith("magiskboot", StringComparison.Ordinal));
            Assert.Contains(native.ShellCommands, command => command.Contains("extract lib/modules/modules.load", StringComparison.Ordinal));
            Assert.Contains(native.ShellCommands, command => command.Contains("extract lib/modules/5.15.148-gki/modules.load", StringComparison.Ordinal));
            Assert.Contains(native.ShellCommands, command => command.Contains("softdep[[:space:]]", StringComparison.Ordinal));
            Assert.Contains(native.ShellCommands, command => command.Contains("repack vendor_boot.img", StringComparison.Ordinal));
            Assert.Contains(native.ShellCommands, command => command.StartsWith("rm -rf", StringComparison.Ordinal));
        }
        finally
        {
            Directory.Delete(directory, true);
        }
    }

    [Fact]
    public async Task PatchAsync_keeps_processing_the_official_kernel_when_no_gki_directory_is_present()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var sourcePath = Path.Combine(directory, "vendor_boot.img");
        await File.WriteAllBytesAsync(sourcePath, "vendor-stock"u8.ToArray());
        var source = await InspectAsync(sourcePath);
        var native = new VendorPatchNativeApi { ModuleListing = "lib/modules/modules.load" };
        var backend = new FastbootRsBackend(native);
        var service = new VivoVendorBootProcessor(backend, new VivoRootResourceService(AppContext.BaseDirectory), new QuickFlashService(backend, new FakeFastbootCliRunner(), new OperationLogService()));

        try
        {
            var result = await service.PatchAsync("ADB123", source, CancellationToken.None);

            Assert.True(File.Exists(result.Path));
            Assert.Contains(native.ShellCommands, command => command.Contains("extract lib/modules/modules.load", StringComparison.Ordinal));
            Assert.DoesNotContain(native.ShellCommands, command => command.Contains("-gki/modules.load", StringComparison.Ordinal));
        }
        finally
        {
            Directory.Delete(directory, true);
        }
    }

    [Fact]
    public async Task PatchAsync_reports_correlated_vendor_boot_stages_when_coordinated()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var sourcePath = Path.Combine(directory, "vendor_boot.img");
        await File.WriteAllBytesAsync(sourcePath, "vendor-stock"u8.ToArray());
        var source = await InspectAsync(sourcePath);
        var native = new VendorPatchNativeApi();
        var logs = new OperationLogService();
        var backend = new FastbootRsBackend(native);
        var service = new VivoVendorBootProcessor(
            backend,
            new VivoRootResourceService(AppContext.BaseDirectory),
            new QuickFlashService(backend, new FakeFastbootCliRunner(), logs));
        var coordinator = new OperationCoordinator(new DeviceSessionViewModel(), logs);

        try
        {
            await coordinator.RunAsync(OperationKind.Hashing, "正在修补 vendor_boot 镜像", async (context, token) =>
            {
                await service.PatchAsync("ADB123", source, token, context);
            });

            var operationId = Assert.Single(logs.Entries, entry => entry.Message == "正在修补 vendor_boot 镜像").OperationId;
            Assert.NotNull(operationId);
            Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在上传 vendor_boot", StringComparison.Ordinal));
            Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在处理 vendor_boot", StringComparison.Ordinal));
            Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在获取修补后的 vendor_boot", StringComparison.Ordinal));
        }
        finally
        {
            Directory.Delete(directory, true);
        }
    }

    private static async Task<FlashImageInfo> InspectAsync(string path)
    {
        return new FlashImageInfo(path, new FileInfo(path).Length);
    }

    private sealed class VendorPatchNativeApi : IFastbootRsNativeApi
    {
        public string ModuleListing { get; init; } = "lib/modules/5.15.148-gki\nlib/modules/modules.load";
        public List<string> PushedRemotePaths { get; } = [];
        public List<string> ShellCommands { get; } = [];

        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command)
        {
            ShellCommands.Add(command);
            if (command.Contains("ls /lib/modules/", StringComparison.Ordinal)) return ModuleListing;
            if (command.Contains("unpack vendor_boot.img", StringComparison.Ordinal)) return "VENDOR_RAMDISK_READY";
            if (command.Contains("extract ", StringComparison.Ordinal)) return "READY";
            if (command.Contains("repack vendor_boot.img", StringComparison.Ordinal)) return "REPACKED";
            if (command.Contains("test -d", StringComparison.Ordinal)) return "READY";
            if (command.Contains("test -f", StringComparison.Ordinal)) return "READY";
            return string.Empty;
        }

        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) => PushedRemotePaths.Add(remotePath);
        public long Pull(string? serial, string remotePath, string localPath)
        {
            File.WriteAllBytes(localPath, "vendor-patched"u8.ToArray());
            return new FileInfo(localPath).Length;
        }

        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
