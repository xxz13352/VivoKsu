using System.Collections;
using System.IO.Compression;
using CommunityToolkit.Mvvm.Input;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class LineFlashViewModelTests
{
    [Fact]
    public async Task RefreshAsync_populates_partition_rows_for_a_fastboot_device()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, "FAST123", "Fastboot 已连接"));
        var backend = new FastbootRsBackend(new LineFlashNativeApi());
        var logs = new OperationLogService();
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable switch
            {
                "current-slot" => "a",
                "is-userspace" => "no",
                "partition-size:boot" => "0x04000000",
                "partition-size:init_boot" => "0x00800000",
                "partition-size:vendor_boot" => "0x06000000",
                _ => string.Empty
            }
        };

        var viewModel = CreateViewModel(session, backend, logs, fake);
        await (Task)viewModel.GetType().GetMethod("RefreshAsync")!.Invoke(viewModel, [true])!;

        var partitions = (IEnumerable)viewModel.GetType().GetProperty("Partitions")!.GetValue(viewModel)!;
        var boot = partitions.Cast<object>().Single(row => (string)Get(row, "Name") == "boot");

        Assert.Equal("64 MB", Get(boot, "SizeDisplay"));
        Assert.Equal("分区表已更新", session.StatusText);
        Assert.Contains(logs.Entries, entry => entry.Message.Contains("分区表", StringComparison.Ordinal));
    }

    [Fact]
    public async Task RefreshAsync_resets_the_table_when_the_device_is_not_in_fastboot()
    {
        var session = new DeviceSessionViewModel();
        var backend = new FastbootRsBackend(new LineFlashNativeApi());
        var logs = new OperationLogService();

        var viewModel = CreateViewModel(session, backend, logs);
        await (Task)viewModel.GetType().GetMethod("RefreshAsync")!.Invoke(viewModel, [true])!;

        Assert.Equal("等待 Fastboot 设备", Get(viewModel, "TableStatusText"));
        Assert.Contains(logs.Entries, entry => entry.Message.Contains("Fastboot", StringComparison.Ordinal));
    }

    [Fact]
    public async Task ExtractToQuickFlashCommand_stages_a_managed_image_and_calls_the_continuation()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var packagePath = Path.Combine(root, "firmware.zip");
        Directory.CreateDirectory(root);

        try
        {
            using (var archive = ZipFile.Open(packagePath, ZipArchiveMode.Create))
            {
                using var writer = new StreamWriter(archive.CreateEntry("images/init_boot.img").Open());
                writer.Write("init-boot-payload");
            }

            var session = new DeviceSessionViewModel();
            var backend = new FastbootRsBackend(new LineFlashNativeApi());
            var logs = new OperationLogService();
            var viewModel = CreateViewModel(session, backend, logs);
            var package = await new FirmwarePackageInspector().InspectAsync(packagePath, CancellationToken.None);
            viewModel.GetType().GetProperty("VivoPackage")!.SetValue(viewModel, package);

            var selectedEntry = viewModel.GetType().GetProperty("SelectedManagedImageEntry");
            Assert.NotNull(selectedEntry);
            selectedEntry!.SetValue(viewModel, "images/init_boot.img");

            FlashImageInfo? capturedImage = null;
            QuickFlashPartition? capturedPartition = null;
            var continuation = viewModel.GetType().GetMethod("SetQuickFlashContinuation");
            Assert.NotNull(continuation);
            continuation!.Invoke(viewModel, [(Action<FlashImageInfo, QuickFlashPartition>)((image, partition) =>
            {
                capturedImage = image;
                capturedPartition = partition;
            })]);

            var command = viewModel.GetType().GetProperty("ExtractToQuickFlashCommand");
            Assert.NotNull(command);
            await ((IAsyncRelayCommand)command!.GetValue(viewModel)!).ExecuteAsync(null);

            Assert.NotNull(capturedImage);
            Assert.True(File.Exists(capturedImage!.Path));
            Assert.Equal(QuickFlashPartition.InitBoot, capturedPartition);
            Assert.Contains(logs.Entries, entry => entry.Message.Contains("快速刷写", StringComparison.Ordinal));
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    private static object CreateViewModel(
        DeviceSessionViewModel session,
        FastbootRsBackend backend,
        OperationLogService logs,
        IFastbootCliRunner? cliRunner = null)
    {
        var type = typeof(MainViewModel).Assembly.GetType("VivoKsu.App.ViewModels.LineFlashViewModel");
        Assert.NotNull(type);
        return Activator.CreateInstance(type!, session, new FastbootPartitionService(cliRunner ?? new FakeFastbootCliRunner()), logs)!;
    }

    private static object Get(object value, string property) =>
        value.GetType().GetProperty(property)!.GetValue(value)!;

    private sealed class LineFlashNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => "FAST123\tfastboot\n";
        public string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => variable switch
        {
            "current-slot" => "a",
            "is-userspace" => "no",
            "partition-size:boot" => "0x04000000",
            "partition-size:init_boot" => "0x00800000",
            "partition-size:vendor_boot" => "0x06000000",
            _ => string.Empty
        };
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
