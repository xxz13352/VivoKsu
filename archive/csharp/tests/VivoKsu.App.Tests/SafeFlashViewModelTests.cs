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
            logs.Entries.Should().Contain(entry => entry.Message.Contains("分区 1/1 写入完成"));
            logs.Entries.Should().Contain(entry => entry.Message.Contains("任务结束"));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_keeps_extracted_images_when_the_flash_fails()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01, 0x02]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var api = new FlashApi();
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner { FailPartition = "boot" };
            var viewModel = CreateViewModel(session, api, logs, fake);
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            var staging = Path.Combine(directory, "staging");
            viewModel.SetPendingSourceForTesting(zip, staging, partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            // 设备断开/刷写失败:解包好的镜像保留(不 CleanupStaging),并给用户提示路径。
            Directory.Exists(staging).Should().BeTrue();
            Directory.Exists(Path.Combine(staging, "extract")).Should().BeTrue();
            logs.Entries.Should().Contain(entry => entry.Message.Contains("已保留解包好的镜像"));
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
            logs.Entries.Should().Contain(entry => entry.Message.Contains("已连接设备"));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task Start_while_another_operation_runs_is_rejected_without_cancelling_the_active_operation()
    {
        // 回归:另一页面有任务在跑时,本页点刷写不排队、不开始,而是明确提示"已有任务
        // 正在进行中",并且不干扰正在跑的操作(互不干扰)。
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var activeStarted = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        Task? activeOperation = null;
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            using var coordinator = new OperationCoordinator(session, logs);
            activeOperation = coordinator.RunAsync(OperationKind.Transferring, "其他操作", async (_, ct) =>
            {
                activeStarted.TrySetResult();
                await release.Task.WaitAsync(ct);
            });
            await activeStarted.Task.WaitAsync(TimeSpan.FromSeconds(5));

            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake, coordinator);
            viewModel.SetPendingSourceForTesting(
                zip,
                Path.Combine(directory, "staging"),
                [new PayloadPartitionEntry("boot", 1, "none")]);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            viewModel.StatusText.Should().Be(OperationCoordinator.OperationInProgressMessage);
            fake.FlashRequests.Should().BeEmpty();
            fake.Rebooted.Should().BeEmpty();
            activeOperation.IsCompleted.Should().BeFalse();
            viewModel.IsBusy.Should().BeFalse();

            release.TrySetResult();
            await activeOperation.WaitAsync(TimeSpan.FromSeconds(5));
        }
        finally
        {
            release.TrySetResult();
            if (activeOperation is not null)
            {
                try
                {
                    await activeOperation;
                }
                catch (OperationCanceledException)
                {
                }
            }

            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task StopCommand_when_idle_cancels_the_globally_running_operation_from_another_page()
    {
        // 回归:别的页面有任务在跑时,本页即使没有自己的任务,「停止操作」也应可用并
        // 能取消全局正在运行的操作——否则用户在其他菜单点取消会"没反应"。
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, "FB123", "fastboot", "vivo"));
        var logs = new OperationLogService();
        using var coordinator = new OperationCoordinator(session, logs);
        var activeStarted = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var activeCancelled = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var activeOperation = coordinator.RunAsync(OperationKind.Transferring, "其它页面任务", async (_, ct) =>
        {
            activeStarted.TrySetResult();
            try
            {
                await Task.Delay(Timeout.InfiniteTimeSpan, ct);
            }
            catch (OperationCanceledException)
            {
                activeCancelled.TrySetResult();
                throw;
            }
        });
        await activeStarted.Task.WaitAsync(TimeSpan.FromSeconds(5));

        var viewModel = CreateViewModel(session, new FlashApi(), logs, coordinator: coordinator);

        viewModel.StopCommand.CanExecute(null).Should().BeTrue();
        viewModel.StopCommand.Execute(null);
        await activeCancelled.Task.WaitAsync(TimeSpan.FromSeconds(2));

        try
        {
            await activeOperation.WaitAsync(TimeSpan.FromSeconds(5));
        }
        catch (OperationCanceledException)
        {
        }
    }

    [Fact]
    public async Task Direct_coordinator_cancel_cancels_the_active_delegate()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, "FB123", "fastboot", "vivo"));
        var logs = new OperationLogService();
        using var coordinator = new OperationCoordinator(session, logs);
        var activeStarted = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var activeCancelled = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var activeOperation = coordinator.RunAsync(OperationKind.Transferring, "其它页面任务", async (_, ct) =>
        {
            activeStarted.TrySetResult();
            try
            {
                await Task.Delay(Timeout.InfiniteTimeSpan, ct);
            }
            catch (OperationCanceledException)
            {
                activeCancelled.TrySetResult();
                throw;
            }
        });
        await activeStarted.Task.WaitAsync(TimeSpan.FromSeconds(5));

        coordinator.CancelCurrent();
        await activeCancelled.Task.WaitAsync(TimeSpan.FromSeconds(2));

        try
        {
            await activeOperation.WaitAsync(TimeSpan.FromSeconds(5));
        }
        catch (OperationCanceledException)
        {
        }
    }

    [Fact]
    public async Task Start_command_while_another_operation_runs_is_rejected_instead_of_queueing()
    {
        // 回归:全局协调器忙时,开始按钮保持可点,点击给出"已有任务正在进行中"的明确
        // 提示(而不是静默排队),且不启动新的刷写。
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.FastbootConnected, "FB123", "fastboot", "vivo"));
        var logs = new OperationLogService();
        using var coordinator = new OperationCoordinator(session, logs);
        var activeStarted = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var activeOperation = coordinator.RunAsync(OperationKind.Transferring, "其它页面任务", async (_, ct) =>
        {
            activeStarted.TrySetResult();
            await release.Task.WaitAsync(ct);
        });
        await activeStarted.Task.WaitAsync(TimeSpan.FromSeconds(5));

        var fake = new FakeFastbootCliRunner();
        var viewModel = CreateViewModel(session, new FlashApi(), logs, fake, coordinator);
        viewModel.SetPendingSourceForTesting(@"C:\x\ota.zip", @"C:\x\staging",
            [new PayloadPartitionEntry("boot", 1, "none")]);

        viewModel.ConfirmFlashCommand.CanExecute(null).Should().BeTrue();
        await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

        viewModel.StatusText.Should().Be(OperationCoordinator.OperationInProgressMessage);
        fake.FlashRequests.Should().BeEmpty();

        release.TrySetResult();
        await activeOperation.WaitAsync(TimeSpan.FromSeconds(5));
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

    [Fact]
    public async Task ConfirmFlashAsync_safe_flash_off_flashes_preloader_and_lk()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
                CreateEntry(archive, "lk.img", [0x02]);
                CreateEntry(archive, "preloader.img", [0x03]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.IsSafeFlash = false;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition)
                .Should().BeEquivalentTo(["boot", "lk", "preloader"]);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_keep_root_skips_boot_partitions()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
                CreateEntry(archive, "init_boot.img", [0x02]);
                CreateEntry(archive, "vendor_boot.img", [0x03]);
                CreateEntry(archive, "system.img", [0x04]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.IsKeepRoot = true;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["system"]);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_other_slot_flashes_target_slot_and_switches_active()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner
            {
                GetVarHandler = variable => variable switch
                {
                    "current-slot" => "a",
                    _ when variable.StartsWith("has-slot:", StringComparison.OrdinalIgnoreCase) => "yes",
                    _ => string.Empty
                }
            };
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.SlotMode = SafeFlashSlotMode.OtherSlot;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot_b"]);
            fake.SetActiveSlots.Should().Contain("b");
            fake.Rebooted.Should().Contain("FB123");
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_both_slots_flashes_a_and_b_once_each()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
                CreateEntry(archive, "system.img", [0x02]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner
            {
                GetVarHandler = variable => variable.StartsWith("has-slot:", StringComparison.OrdinalIgnoreCase) ? "yes" : string.Empty
            };
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.SlotMode = SafeFlashSlotMode.BothSlots;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition)
                .Should().BeEquivalentTo(["boot_a", "boot_b", "system_a", "system_b"]);
            fake.SetActiveSlots.Should().BeEmpty();
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_non_ab_device_degrades_to_plain_flash()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner(); // GetVar 默认返回空 → current-slot/has-slot 读不到
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.SlotMode = SafeFlashSlotMode.OtherSlot;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            viewModel.SetPendingSourceForTesting(zip, Path.Combine(directory, "staging"), partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot"]);
            fake.SetActiveSlots.Should().BeEmpty();
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    [Fact]
    public async Task ConfirmFlashAsync_wipe_data_flashes_misc_last()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var zip = Path.Combine(directory, "ota.zip");
            using (var archive = ZipFile.Open(zip, ZipArchiveMode.Create))
            {
                CreateEntry(archive, "boot.img", [0x01]);
            }

            var session = new DeviceSessionViewModel();
            session.ApplyDevice(new DeviceSnapshot(
                DeviceConnectionState.FastbootConnected, "FB123", "fastboot 已连接", "vivo"));
            var logs = new OperationLogService();
            var fake = new FakeFastbootCliRunner();
            var viewModel = CreateViewModel(session, new FlashApi(), logs, fake);
            viewModel.IsWipeData = true;
            var extractor = new FirmwarePartitionExtractor(payloadDumper: null);
            var partitions = await extractor.ListPartitionsAsync(zip, CancellationToken.None);
            var staging = Path.Combine(directory, "staging");
            viewModel.SetPendingSourceForTesting(zip, staging, partitions);

            await viewModel.ConfirmFlashCommand.ExecuteAsync(null);

            fake.FlashRequests.Select(request => request.Partition).Should().BeEquivalentTo(["boot", "misc"]);
            fake.FlashRequests.Last().ImagePath.Should().EndWith("wipe-data.img");
            fake.Rebooted.Should().Contain("FB123");
            logs.Entries.Should().Contain(entry => entry.Message.Contains("数据清除完成"));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }

    private static SafeFlashViewModel CreateViewModel(
        DeviceSessionViewModel session,
        FlashApi api,
        OperationLogService? logs = null,
        FakeFastbootCliRunner? fake = null,
        IOperationCoordinator? coordinator = null)
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
            coordinator,
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

        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;

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

        public void FastbootReboot(string? serial, string? target) => FastbootRebootCalled = true;

        public void Push(string? serial, string localPath, string remotePath, int timeoutMilliseconds = 15000)
        {
        }

        public long Pull(string? serial, string remotePath, string localPath, int timeoutMilliseconds = 15000) => 0;

        public string Install(string? serial, string apkPath, bool replace) => string.Empty;

        public void Flash(string? serial, string partition, string imagePath) => FlashedPartitions.Add(partition);
    }
}
