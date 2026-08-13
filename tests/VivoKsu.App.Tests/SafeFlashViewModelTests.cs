using System.IO.Compression;
using FluentAssertions;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class SafeFlashViewModelTests
{
    [Fact]
    public async Task ConfirmFlashAsync_extracts_and_flashes_all_partitions_except_preloader_and_lk()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01, 0x02]);
                CreateEntry(archive, "lk.img", [0x03]);
                CreateEntry(archive, "preloader.img", [0x04]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var api = new FlashApi();
            var logs = new OperationLogService();
            var viewModel = CreateViewModel(session, api, logs);
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            api.FlashedPartitions.Should().BeEquivalentTo(["boot"]);
            api.FastbootRebootCalled.Should().BeTrue();
            viewModel.StatusText.Should().Contain("已刷入");
            logs.Entries.Should().Contain(entry => entry.Message.Contains("Sending 'boot'"));
            logs.Entries.Should().Contain(entry => entry.Message.Contains("Finished. Total time:"));
            logs.Entries.Should().Contain(entry => entry.Message.Contains("任务结束"));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_skips_partitions_not_present_on_the_device()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
                CreateEntry(archive, "dtbo.img", [0x02]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var api = new FlashApi { MissingPartitions = ["dtbo"] };
            var viewModel = CreateViewModel(session, api);
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            api.FlashedPartitions.Should().BeEquivalentTo(["boot"]);
            viewModel.StatusText.Should().Contain("跳过");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public void CancelFlash_hides_the_confirmation_panel()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, "FB123", "fastboot", "vivo"));
        var viewModel = CreateViewModel(session, new FlashApi());
        viewModel.SetPendingSourceForTesting(@"C:\x\ota.zip", @"C:\x\staging",
            [new PayloadPartitionEntry("boot", 10, "none")]);

        viewModel.IsConfirmVisible.Should().BeTrue();
        viewModel.CancelFlashCommand.Execute(null);

        viewModel.IsConfirmVisible.Should().BeFalse();
        viewModel.FlashCount.Should().Be(0);
    }

    [Fact]
    public void DownloadAndFlashCommand_requires_an_adb_device()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.Disconnected, "--", "未连接"));
        var viewModel = CreateViewModel(session, new FlashApi());

        viewModel.DownloadAndFlashCommand.CanExecute(null).Should().BeFalse();
    }

    [Fact]
    public void ParseBbkVersion_extracts_codename_and_version()
    {
        var (codename, version) = SafeFlashViewModel.ParseBbkVersion("DPD2221B_A_15.2.12.0.W10.V000L1");

        codename.Should().Be("DPD2221B");
        version.Should().Be("15.2.12.0.W10.V000L1");
    }

    [Theory]
    [InlineData("DPD2221B_15.2.12.0.W10.V000L1")]
    [InlineData("DPD2221B")]
    public void ParseBbkVersion_handles_variants_without_failing(string value)
    {
        var (codename, version) = SafeFlashViewModel.ParseBbkVersion(value);

        codename.Should().NotBeNull();
        version.Should().NotBeNull();
    }

    private static SafeFlashViewModel CreateViewModel(
        DeviceSessionViewModel session,
        FlashApi api,
        OperationLogService? logs = null)
    {
        logs ??= new OperationLogService();
        var backend = new FastbootRsBackend(api);
        return new SafeFlashViewModel(
            session,
            logs,
            backend,
            new OtaApiClient(),
            new OtaDownloadService(),
            new FirmwarePartitionExtractor(payloadDumper: null),
            coordinator: null);
    }

    private static void CreateEntry(ZipArchive archive, string name, byte[] content)
    {
        var entry = archive.CreateEntry(name);
        using var stream = entry.Open();
        stream.Write(content);
    }

    private sealed class FlashApi : IFastbootRsNativeApi
    {
        public List<string> FlashedPartitions { get; } = [];

        public HashSet<string> MissingPartitions { get; set; } = [];

        public bool FastbootRebootCalled { get; private set; }

        public List<string> RebootTargets { get; } = [];

        public string ListDevices() => string.Empty;

        public string Shell(string? serial, string command) => string.Empty;

        public string GetVar(string? serial, string variable)
        {
            var partition = variable.Split(':', 2)[^1];
            return MissingPartitions.Contains(partition) ? string.Empty : "raw";
        }

        public void Reboot(string? serial, string target) => RebootTargets.Add(target);

        public void FastbootReboot(string? serial) => FastbootRebootCalled = true;

        public void Push(string? serial, string localPath, string remotePath)
        {
        }

        public long Pull(string? serial, string remotePath, string localPath) => 0;

        public string Install(string? serial, string apkPath, bool replace) => string.Empty;

        public void Flash(string? serial, string partition, string imagePath) => FlashedPartitions.Add(partition);
    }
}
