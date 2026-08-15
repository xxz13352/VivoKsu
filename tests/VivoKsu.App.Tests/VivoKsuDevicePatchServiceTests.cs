using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public sealed class VivoKsuDevicePatchServiceTests
{
    [Fact]
    public async Task PatchAsync_uses_the_selected_manager_and_kmi_to_patch_init_boot_over_adb()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var sourcePath = Path.Combine(directory, "init_boot.img");
        await File.WriteAllBytesAsync(sourcePath, "stock-image"u8.ToArray());
        var source = await InspectAsync(sourcePath);
        var native = new DevicePatchNativeApi("patched-image"u8.ToArray());
        var backend = new FastbootRsBackend(native);
        var resources = new VivoRootResourceService(AppContext.BaseDirectory);
        var service = new VivoKsuDevicePatchService(backend, resources, new QuickFlashService(backend, new FakeFastbootCliRunner(), new OperationLogService()));

        try
        {
            var result = await service.PatchAsync("ADB123", "KSU", "android14-6.1", source, CancellationToken.None);

            Assert.True(File.Exists(result.Path));
            Assert.Equal("init_boot_vivoksu_patched.img", Path.GetFileName(result.Path));
            Assert.Contains(native.PushedRemotePaths, path => path.EndsWith("libksud.so", StringComparison.Ordinal));
            Assert.Contains(native.PushedRemotePaths, path => path.EndsWith("init_boot.img", StringComparison.Ordinal));
            Assert.Contains(native.ShellCommands, command => command.Contains("--partition init_boot --kmi android14-6.1", StringComparison.Ordinal));
            Assert.Contains(native.ShellCommands, command => command.StartsWith("rm -f", StringComparison.Ordinal));
        }
        finally
        {
            Directory.Delete(directory, true);
        }
    }

    [Fact]
    public async Task PatchAsync_accepts_a_source_image_that_changed_after_preflight()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var sourcePath = Path.Combine(directory, "init_boot.img");
        await File.WriteAllBytesAsync(sourcePath, "stock-image"u8.ToArray());
        var source = await InspectAsync(sourcePath);
        await File.WriteAllBytesAsync(sourcePath, "modified-image"u8.ToArray());
        var native = new DevicePatchNativeApi("patched-image"u8.ToArray());
        var backend = new FastbootRsBackend(native);
        var service = new VivoKsuDevicePatchService(
            backend,
            new VivoRootResourceService(AppContext.BaseDirectory),
            new QuickFlashService(backend, new FakeFastbootCliRunner(), new OperationLogService()));

        try
        {
            var result = await service.PatchAsync("ADB123", "KSU", "android14-6.1", source, CancellationToken.None);

            Assert.True(File.Exists(result.Path));
            Assert.Contains(native.PushedRemotePaths, path => path.EndsWith("init_boot.img", StringComparison.Ordinal));
        }
        finally
        {
            Directory.Delete(directory, true);
        }
    }

    [Fact]
    public async Task PatchAsync_reports_correlated_init_boot_stages_when_coordinated()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var sourcePath = Path.Combine(directory, "init_boot.img");
        await File.WriteAllBytesAsync(sourcePath, "stock-image"u8.ToArray());
        var source = await InspectAsync(sourcePath);
        var native = new DevicePatchNativeApi("patched-image"u8.ToArray());
        var logs = new OperationLogService();
        var backend = new FastbootRsBackend(native);
        var service = new VivoKsuDevicePatchService(
            backend,
            new VivoRootResourceService(AppContext.BaseDirectory),
            new QuickFlashService(backend, new FakeFastbootCliRunner(), logs));
        var coordinator = new OperationCoordinator(new DeviceSessionViewModel(), logs);

        try
        {
            await coordinator.RunAsync(OperationKind.Hashing, "正在修补 ROOT 镜像", async (context, token) =>
            {
                await service.PatchAsync("ADB123", "KSU", "android14-6.1", source, token, context);
            });

            var operationId = Assert.Single(logs.Entries, entry => entry.Message == "正在修补 ROOT 镜像").OperationId;
            Assert.NotNull(operationId);
            Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在准备 ROOT 修补资源", StringComparison.Ordinal));
            Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在上传 init_boot", StringComparison.Ordinal));
            Assert.Contains(logs.Entries, entry => entry.OperationId == operationId && entry.Message.Contains("正在获取修补后的 init_boot", StringComparison.Ordinal));
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

    private sealed class DevicePatchNativeApi(byte[] patchedPayload) : IFastbootRsNativeApi
    {
        public List<string> PushedRemotePaths { get; } = [];
        public List<string> ShellCommands { get; } = [];

        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000)
        {
            ShellCommands.Add(command);
            return string.Empty;
        }

        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) => PushedRemotePaths.Add(remotePath);
        public long Pull(string? serial, string remotePath, string localPath)
        {
            File.WriteAllBytes(localPath, patchedPayload);
            return patchedPayload.Length;
        }

        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
