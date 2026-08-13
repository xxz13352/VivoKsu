using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class QuickFlashViewModelTests
{
    [Fact]
    public void Presets_and_flash_options_match_the_compact_reference_layout()
    {
        var viewModel = CreateViewModel();

        Assert.Equal(
        [
            QuickFlashPartition.Boot,
            QuickFlashPartition.InitBoot,
            QuickFlashPartition.VendorBoot,
            QuickFlashPartition.Lk
        ],
        viewModel.Presets.Select(item => item.Partition));
        Assert.True(viewModel.AutoReboot);
        Assert.True(viewModel.WaitForDevice);
        Assert.False(viewModel.FlashBothSlots);
        Assert.False(viewModel.SwitchSlotAfterFlash);
        Assert.False(viewModel.CanSwitchSlotAfterFlash);
        Assert.Equal(["boot", "init_boot", "vendor_boot", "lk"], viewModel.Presets.Select(item => item.DisplayName));
    }

    [Fact]
    public void RequestFlashCommand_shows_confirmation_only_after_an_image_has_been_selected()
    {
        var viewModel = new QuickFlashViewModel(
            new DeviceSessionViewModel(),
            new QuickFlashService(new FastbootRsBackend(new EmptyNativeApi()), new OperationLogService()),
            new OperationLogService());

        Assert.False(viewModel.RequestFlashCommand.CanExecute(null));

        viewModel.SelectedImage = new FlashImageInfo("C:\\images\\boot.img", 1024);
        viewModel.RequestFlashCommand.Execute(null);

        Assert.True(viewModel.IsConfirmationVisible);
    }

    [Fact]
    public void PreparePatchedImage_prefills_the_verified_image_and_requested_partition()
    {
        var viewModel = new QuickFlashViewModel(
            new DeviceSessionViewModel(),
            new QuickFlashService(new FastbootRsBackend(new EmptyNativeApi()), new OperationLogService()),
            new OperationLogService());
        var image = new FlashImageInfo("C:\\images\\init_boot_ksu_patched.img", 1024);

        viewModel.PreparePatchedImage(image, QuickFlashPartition.InitBoot);

        Assert.Same(image, viewModel.SelectedImage);
        Assert.Equal(QuickFlashPartition.InitBoot, viewModel.SelectedPartition);
        Assert.False(viewModel.IsConfirmationVisible);
    }

    [Fact]
    public async Task CancelActiveFlashCommand_stops_the_waiting_flash_operation()
    {
        var native = new WaitingNativeApi();
        var logs = new OperationLogService();
        var session = new DeviceSessionViewModel();
        var viewModel = new QuickFlashViewModel(
            session,
            new QuickFlashService(new FastbootRsBackend(native), logs),
            logs)
        {
            SelectedImage = new FlashImageInfo("C:\\images\\boot.img", 1024)
        };

        var operation = viewModel.ConfirmFlashCommand.ExecuteAsync(null);
        await native.DiscoveryStarted.Task.WaitAsync(TimeSpan.FromSeconds(2));

        Assert.True(viewModel.IsFlashOperationActive);
        Assert.True(viewModel.CancelActiveFlashCommand.CanExecute(null));

        viewModel.CancelActiveFlashCommand.Execute(null);
        await operation;

        Assert.False(viewModel.IsFlashOperationActive);
        Assert.Equal(OperationKind.Canceled, session.OperationKind);
        Assert.Equal("快速刷写已取消", session.StatusText);
        Assert.Contains(logs.Entries, entry => entry.Level == OperationLogLevel.Warning && entry.Message.Contains("已取消"));
    }

    [Fact]
    public async Task CancelActiveFlashCommand_cancels_through_the_shared_coordinator()
    {
        var native = new WaitingNativeApi();
        var logs = new OperationLogService();
        var session = new DeviceSessionViewModel();
        var coordinator = new OperationCoordinator(session, logs);
        var viewModel = new QuickFlashViewModel(
            session,
            new QuickFlashService(new FastbootRsBackend(native), logs),
            logs,
            coordinator)
        {
            SelectedImage = new FlashImageInfo("C:\\images\\boot.img", 1024)
        };

        var operation = viewModel.ConfirmFlashCommand.ExecuteAsync(null);
        await native.DiscoveryStarted.Task.WaitAsync(TimeSpan.FromSeconds(2));

        viewModel.CancelActiveFlashCommand.Execute(null);
        await operation;

        Assert.Equal(OperationKind.Canceled, session.OperationKind);
        Assert.False(coordinator.IsBusy);
    }

    [Fact]
    public void Turning_off_dual_slot_also_turns_off_switch_slot()
    {
        var viewModel = CreateViewModel();
        viewModel.FlashBothSlots = true;
        viewModel.SwitchSlotAfterFlash = true;

        viewModel.FlashBothSlots = false;

        Assert.False(viewModel.SwitchSlotAfterFlash);
        Assert.False(viewModel.CanSwitchSlotAfterFlash);
    }

    [Fact]
    public void RequestBatchFlash_snapshots_only_rows_with_images()
    {
        var viewModel = CreateViewModel();
        Find(viewModel, QuickFlashPartition.Boot).SelectedImage = new("C:\\images\\boot.img", 10);
        Find(viewModel, QuickFlashPartition.VendorBoot).SelectedImage = new("C:\\images\\vendor_boot.bin", 20);

        viewModel.RequestBatchFlashCommand.Execute(null);
        Find(viewModel, QuickFlashPartition.Boot).SelectedImage = null;

        Assert.True(viewModel.IsConfirmationVisible);
        Assert.Equal(
            [QuickFlashPartition.Boot, QuickFlashPartition.VendorBoot],
            viewModel.PendingPlan!.Requests.Select(request => request.Partition));
        Assert.Equal(2, viewModel.PendingPlan.Requests.Count);
    }

    [Fact]
    public void RequestPresetFlash_snapshots_only_the_requested_row()
    {
        var viewModel = CreateViewModel();
        var boot = Find(viewModel, QuickFlashPartition.Boot);
        boot.SelectedImage = new("C:\\images\\boot.img", 10);
        Find(viewModel, QuickFlashPartition.VendorBoot).SelectedImage = new("C:\\images\\vendor.img", 20);

        viewModel.RequestPresetFlashCommand.Execute(boot);

        Assert.Equal(QuickFlashPartition.Boot, Assert.Single(viewModel.PendingPlan!.Requests).Partition);
    }

    [Fact]
    public void Confirmation_summary_describes_the_frozen_dual_slot_plan()
    {
        var viewModel = CreateViewModel();
        Find(viewModel, QuickFlashPartition.Boot).SelectedImage = new("C:\\images\\boot.img", 10);
        Find(viewModel, QuickFlashPartition.InitBoot).SelectedImage = new("C:\\images\\init_boot.img", 20);
        viewModel.FlashBothSlots = true;
        viewModel.SwitchSlotAfterFlash = true;
        viewModel.AutoReboot = false;

        viewModel.RequestBatchFlashCommand.Execute(null);

        Assert.Contains("2 个分区", viewModel.ConfirmationSummary);
        Assert.Contains("双槽", viewModel.ConfirmationSummary);
        Assert.Contains("切换槽位", viewModel.ConfirmationSummary);
        Assert.Contains("不自动重启", viewModel.ConfirmationSummary);
    }

    [Fact]
    public void Flash_commands_re_enable_after_a_busy_state_clears()
    {
        var logs = new OperationLogService();
        var session = new DeviceSessionViewModel();
        var viewModel = new QuickFlashViewModel(
            session,
            new QuickFlashService(new FastbootRsBackend(new EmptyNativeApi()), logs),
            logs);
        var preset = Find(viewModel, QuickFlashPartition.Boot);
        preset.SelectedImage = new FlashImageInfo(@"D:\firmware\boot.img", 64L * 1024 * 1024);

        // The file-selection flow sets IsBusy before SelectedImage updates, so the
        // command is evaluated while busy and must re-enable once IsBusy clears.
        session.BeginOperation(OperationKind.Hashing, "正在读取镜像");
        preset.SelectedImage = new FlashImageInfo(@"D:\firmware\boot.img", 64L * 1024 * 1024);
        Assert.False(viewModel.RequestBatchFlashCommand.CanExecute(null));

        session.CompleteOperation("镜像读取完成");

        Assert.True(viewModel.RequestBatchFlashCommand.CanExecute(null));
        Assert.True(viewModel.RequestPresetFlashCommand.CanExecute(preset));
    }

    private static QuickFlashViewModel CreateViewModel()
    {
        var logs = new OperationLogService();
        return new QuickFlashViewModel(
            new DeviceSessionViewModel(),
            new QuickFlashService(new FastbootRsBackend(new EmptyNativeApi()), logs),
            logs);
    }

    private static QuickFlashPresetItemViewModel Find(
        QuickFlashViewModel viewModel,
        QuickFlashPartition partition) =>
        Assert.Single(viewModel.Presets, item => item.Partition == partition);

    private sealed class EmptyNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class WaitingNativeApi : IFastbootRsNativeApi
    {
        public TaskCompletionSource<bool> DiscoveryStarted { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public string ListDevices()
        {
            DiscoveryStarted.TrySetResult(true);
            return string.Empty;
        }

        public string Shell(string? serial, string command) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
