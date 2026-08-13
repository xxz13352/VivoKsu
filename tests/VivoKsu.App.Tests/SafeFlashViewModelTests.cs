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
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, api, logs, fake);
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot"]);
            fake.Rebooted.Should().Contain("FB123");
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
            var fake = new FakeFastbootCliRunner { MissingPartitions = ["dtbo"] };
            var viewModel = CreateViewModel(session, api, fake: fake);
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot"]);
            viewModel.StatusText.Should().Contain("跳过");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_waits_for_fastbootd_after_adb_reboot()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01, 0x02]);
                CreateEntry(archive, "preloader.img", [0x04]);
            }

            // 设备起初在 ADB;adb reboot fastboot 后 FastbootDeviceOutput 出现 fastbootd 设备。
            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.AdbConnected, "ADB123", "adb 已连接", "vivo"));
            var api = new FlashApi { FastbootDeviceOutput = "FB123\tfastbootd" };
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, api, logs, fake);
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            api.RebootTargets.Should().Contain("fastboot");
            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot"]);
            fake.Rebooted.Should().Contain("FB123");
            // WaitForFastbootAsync 应把探测到的 fastboot 设备应用到 session,
            // 否则刷写循环会因 session 仍是 AdbConnected 而误判断开。
            session.ConnectionState.Should().Be(DeviceConnectionState.FastbootConnected);
            session.Serial.Should().Be("FB123");
            viewModel.StatusText.Should().Contain("已刷入");
            logs.Entries.Should().Contain(entry => entry.Message.Contains("已连接 FB123"));
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
        OperationLogService? logs = null,
        FakeFastbootCliRunner? fake = null)
    {
        logs ??= new OperationLogService();
        var backend = new FastbootRsBackend(api);
        fake ??= new FakeFastbootCliRunner { MissingPartitions = [.. api.MissingPartitions] };
        return new SafeFlashViewModel(
            session,
            logs,
            backend,
            new OtaApiClient(),
            new OtaDownloadService(),
            new FirmwarePartitionExtractor(payloadDumper: null),
            coordinator: null,
            fake);
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

        /// <summary>adb reboot fastboot 之后 ListDevices 返回的设备列表。</summary>
        public string FastbootDeviceOutput { get; set; } = string.Empty;

        private bool rebootIssued;

        public string ListDevices() => rebootIssued ? FastbootDeviceOutput : string.Empty;

        public string Shell(string? serial, string command) => string.Empty;

        public string GetVar(string? serial, string variable)
        {
            var partition = variable.Split(':', 2)[^1];
            return MissingPartitions.Contains(partition) ? string.Empty : "raw";
        }

        public void Reboot(string? serial, string target)
        {
            RebootTargets.Add(target);
            rebootIssued = true;
        }

        public void FastbootReboot(string? serial) => FastbootRebootCalled = true;

        public void Push(string? serial, string localPath, string remotePath)
        {
        }

        public long Pull(string? serial, string remotePath, string localPath) => 0;

        public string Install(string? serial, string apkPath, bool replace) => string.Empty;

        public void Flash(string? serial, string partition, string imagePath) => FlashedPartitions.Add(partition);
    }
}
