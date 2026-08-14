using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

/// <summary>快速刷写:预设分区选择镜像后按 fastbootd 刷入(完成可选自动重启/双槽)。</summary>
/// <remarks>VIVO 刷写一律在 fastbootd(userspace fastboot)进行,不再提供 Fastboot/bootloader 选择。</remarks>
public partial class QuickFlashViewModel : ObservableObject
{
    private readonly DeviceSessionViewModel session;
    private readonly QuickFlashService quickFlash;
    private readonly OperationLogService logs;
    private readonly IOperationCoordinator? coordinator;
    private CancellationTokenSource? flashCancellation;

    [ObservableProperty]
    private QuickFlashPartition selectedPartition = QuickFlashPartition.Boot;

    // Retained for existing ROOT handoff callers while the quick-flash UI uses Presets.
    [ObservableProperty]
    private FlashImageInfo? selectedImage;

    [ObservableProperty]
    private bool autoReboot = true;

    [ObservableProperty]
    private bool waitForDevice = true;

    [ObservableProperty]
    private bool flashBothSlots;

    [ObservableProperty]
    private bool switchSlotAfterFlash;

    [ObservableProperty]
    private string currentPartition = "--";

    [ObservableProperty]
    private double currentPartitionProgress;

    [ObservableProperty]
    private double overallProgress;

    [ObservableProperty]
    private string speedText = string.Empty;

    [ObservableProperty]
    private QuickFlashExecutionPlan? pendingPlan;

    [ObservableProperty]
    private bool isConfirmationVisible;

    [ObservableProperty]
    private bool isFlashOperationActive;

    public QuickFlashViewModel(
        DeviceSessionViewModel session,
        QuickFlashService quickFlash,
        OperationLogService logs,
        IOperationCoordinator? coordinator = null)
    {
        this.session = session;
        this.quickFlash = quickFlash;
        this.logs = logs;
        this.coordinator = coordinator;

        // Commands gate on session.IsBusy and coordinator.IsBusy, so a busy-state
        // transition must re-evaluate them; without this a file selection that
        // briefly set IsBusy would leave the flash buttons disabled forever.
        session.PropertyChanged += OnSessionPropertyChanged;
        if (coordinator is not null)
        {
            coordinator.StateChanged += OnCoordinatorStateChanged;
        }

        Presets =
        [
            new(QuickFlashPartition.Boot, "boot"),
            new(QuickFlashPartition.InitBoot, "init_boot"),
            new(QuickFlashPartition.VendorBoot, "vendor_boot"),
            new(QuickFlashPartition.Lk, "lk")
        ];

        foreach (var preset in Presets)
        {
            preset.PropertyChanged += OnPresetPropertyChanged;
        }

        BrowseImageCommand = new AsyncRelayCommand(BrowseImageAsync, CanBrowseLegacyImage);
        BrowsePresetImageCommand = new AsyncRelayCommand<QuickFlashPresetItemViewModel?>(BrowsePresetImageAsync, CanBrowsePresetImage);
        RequestFlashCommand = new RelayCommand(RequestLegacyFlash, CanRequestLegacyFlash);
        RequestPresetFlashCommand = new RelayCommand<QuickFlashPresetItemViewModel?>(RequestPresetFlash, CanRequestPresetFlash);
        RequestBatchFlashCommand = new RelayCommand(RequestBatchFlash, CanRequestBatchFlash);
        ConfirmFlashCommand = new AsyncRelayCommand(ConfirmFlashAsync, CanConfirmFlash);
        CancelFlashCommand = new RelayCommand(CancelConfirmation);
        CancelActiveFlashCommand = new RelayCommand(
            CancelActiveFlash,
            () => IsFlashOperationActive || coordinator?.IsBusy == true);
    }

    public IAsyncRelayCommand BrowseImageCommand { get; }

    public IAsyncRelayCommand<QuickFlashPresetItemViewModel?> BrowsePresetImageCommand { get; }

    public IRelayCommand RequestFlashCommand { get; }

    public IRelayCommand<QuickFlashPresetItemViewModel?> RequestPresetFlashCommand { get; }

    public IRelayCommand RequestBatchFlashCommand { get; }

    public IAsyncRelayCommand ConfirmFlashCommand { get; }

    public IRelayCommand CancelFlashCommand { get; }

    public IRelayCommand CancelActiveFlashCommand { get; }

    public IReadOnlyList<QuickFlashPresetItemViewModel> Presets { get; }

    public bool CanSwitchSlotAfterFlash => FlashBothSlots;

    public string CurrentPartitionProgressPercent =>
        IsFlashOperationActive && CurrentPartitionProgress > 0 ? $"{CurrentPartitionProgress:P0}" : "--";

    public string OverallProgressPercent => IsFlashOperationActive ? $"{OverallProgress:P0}" : "--";

    public bool IsCurrentPartitionIndeterminate => IsFlashOperationActive && CurrentPartitionProgress <= 0;

    public string ImageSizeDisplay => SelectedImage is null ? "未选择镜像" : FormatBytes(SelectedImage.SizeBytes);

    public string ConfirmationSummary => PendingPlan is null
        ? string.Empty
        : BuildConfirmationSummary(PendingPlan);

    public void PreparePatchedImage(FlashImageInfo image, QuickFlashPartition partition)
    {
        SelectedPartition = partition;
        SelectedImage = image;
        Presets.Single(item => item.Partition == partition).SelectedImage = image;
        PendingPlan = null;
        IsConfirmationVisible = false;
    }

    partial void OnSelectedImageChanged(FlashImageInfo? value)
    {
        OnPropertyChanged(nameof(ImageSizeDisplay));
        if (value is not null)
        {
            Presets.Single(item => item.Partition == SelectedPartition).SelectedImage = value;
        }

        NotifyCommandsCanExecuteChanged();
    }

    partial void OnPendingPlanChanged(QuickFlashExecutionPlan? value)
    {
        OnPropertyChanged(nameof(ConfirmationSummary));
        ConfirmFlashCommand.NotifyCanExecuteChanged();
    }

    partial void OnIsConfirmationVisibleChanged(bool value) => NotifyCommandsCanExecuteChanged();

    partial void OnIsFlashOperationActiveChanged(bool value) => NotifyCommandsCanExecuteChanged();

    partial void OnFlashBothSlotsChanged(bool value)
    {
        if (!value)
        {
            SwitchSlotAfterFlash = false;
        }

        OnPropertyChanged(nameof(CanSwitchSlotAfterFlash));
    }

    partial void OnCurrentPartitionProgressChanged(double value)
    {
        OnPropertyChanged(nameof(CurrentPartitionProgressPercent));
        OnPropertyChanged(nameof(IsCurrentPartitionIndeterminate));
    }

    partial void OnOverallProgressChanged(double value) => OnPropertyChanged(nameof(OverallProgressPercent));

    private void OnPresetPropertyChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(QuickFlashPresetItemViewModel.SelectedImage))
        {
            NotifyCommandsCanExecuteChanged();
        }
    }

    private void OnSessionPropertyChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName is nameof(DeviceSessionViewModel.IsBusy) or nameof(DeviceSessionViewModel.ConnectionState))
        {
            NotifyCommandsCanExecuteChanged();
        }
    }

    private void OnCoordinatorStateChanged(object? sender, EventArgs eventArgs) => NotifyCommandsCanExecuteChanged();

    private async Task BrowseImageAsync()
    {
        var preset = Presets.Single(item => item.Partition == SelectedPartition);
        await BrowsePresetImageAsync(preset);
        SelectedImage = preset.SelectedImage;
    }

    private async Task BrowsePresetImageAsync(QuickFlashPresetItemViewModel? item)
    {
        if (item is null || IsFlashOperationActive)
        {
            return;
        }

        var dialog = new OpenFileDialog
        {
            Filter = "Android image (*.img;*.bin)|*.img;*.bin",
            CheckFileExists = true,
            Multiselect = false,
            Title = $"选择 {item.DisplayName} 镜像"
        };

        if (dialog.ShowDialog() != true)
        {
            return;
        }

        session.BeginOperation(OperationKind.Hashing, "正在读取镜像");
        logs.Write(OperationLogLevel.Info, $"正在读取镜像: {Path.GetFileName(dialog.FileName)}。");

        try
        {
            item.SelectedImage = await quickFlash.InspectImageAsync(dialog.FileName, CancellationToken.None);
            session.CompleteOperation("镜像读取完成");
            logs.Write(OperationLogLevel.Success, "镜像读取完成。");
        }
        catch (Exception exception)
        {
            session.FailOperation("镜像读取失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
    }

    private void RequestLegacyFlash()
    {
        if (SelectedImage is null)
        {
            return;
        }

        var preset = Presets.Single(item => item.Partition == SelectedPartition);
        preset.SelectedImage = SelectedImage;
        RequestPresetFlash(preset);
    }

    private void RequestPresetFlash(QuickFlashPresetItemViewModel? item)
    {
        if (item?.SelectedImage is null || IsFlashOperationActive)
        {
            return;
        }

        ShowConfirmation(CreatePlan([item]));
    }

    private void RequestBatchFlash()
    {
        if (IsFlashOperationActive)
        {
            return;
        }

        ShowConfirmation(CreatePlan(Presets));
    }

    private void ShowConfirmation(QuickFlashExecutionPlan plan)
    {
        if (plan.Requests.Count == 0)
        {
            return;
        }

        PendingPlan = plan;
        IsConfirmationVisible = true;
    }

    private QuickFlashExecutionPlan CreatePlan(IEnumerable<QuickFlashPresetItemViewModel> items)
    {
        var requests = items
            .Where(item => item.SelectedImage is not null)
            .Select(item => new QuickFlashRequest(item.Partition, item.SelectedImage!))
            .ToArray();
        return new QuickFlashExecutionPlan(
            requests,
            new QuickFlashOptions(
                // VIVO 刷写一律走 fastbootd(userspace fastboot),bootloader 模式刷不了
                // init_boot/vendor_boot 等分区;不向用户提供选择。
                FastbootTarget.Fastbootd,
                WaitForDevice,
                FlashBothSlots,
                SwitchSlotAfterFlash,
                AutoReboot));
    }

    private async Task ConfirmFlashAsync()
    {
        if (IsFlashOperationActive)
        {
            return;
        }

        // 只刷用户确认面板上看到的计划(PendingPlan)。绝不能按当前 UI 重建:
        // 单预设「刷入」确认的是 1 个分区,重建会扩成所有已加载镜像的预设,
        // 把用户从未确认的分区也刷进去。确认面板打开期间浏览被禁,快照有效。
        // 没有 PendingPlan 时回退到旧的单镜像路径(SelectedImage)。
        var plan = PendingPlan ?? CreateLegacyPlan();
        if (plan is null || plan.Requests.Count == 0)
        {
            CancelConfirmation();
            return;
        }

        PendingPlan = null;
        IsConfirmationVisible = false;
        IsFlashOperationActive = true;
        ResetFlashProgress(plan);
        var flashProgress = new Progress<(string Partition, double Fraction)>(value => OnFlashProgress(value, plan));

        try
        {
            if (coordinator is not null)
            {
                await coordinator.RunAsync(
                    OperationKind.Flashing,
                    $"正在刷写 {plan.Requests.Count} 个预设分区",
                    (context, cancellationToken) => quickFlash.FlashImagesAsync(
                        session,
                        plan.Requests,
                        plan.Options,
                        cancellationToken,
                        context,
                        flashProgress));
            }
            else
            {
                using var cancellation = new CancellationTokenSource();
                flashCancellation = cancellation;
                await quickFlash.FlashImagesAsync(
                    session,
                    plan.Requests,
                    plan.Options,
                    cancellation.Token,
                    flashProgress: flashProgress);
            }
        }
        catch (Exception)
        {
            // The service or coordinator has already recorded the concrete failure in the shared log.
        }
        finally
        {
            flashCancellation = null;
            IsFlashOperationActive = false;
        }
    }

    private QuickFlashExecutionPlan? CreateLegacyPlan()
    {
        if (SelectedImage is null)
        {
            return null;
        }

        return new QuickFlashExecutionPlan(
            [new QuickFlashRequest(SelectedPartition, SelectedImage)],
            new QuickFlashOptions(FastbootTarget.Fastbootd, WaitForDevice, FlashBothSlots, SwitchSlotAfterFlash, AutoReboot));
    }

    private void CancelConfirmation()
    {
        PendingPlan = null;
        IsConfirmationVisible = false;
    }

    private bool CanBrowseLegacyImage() => !IsFlashOperationActive && !session.IsBusy && !IsConfirmationVisible;

    private bool CanBrowsePresetImage(QuickFlashPresetItemViewModel? item) =>
        item is not null && !IsFlashOperationActive && !session.IsBusy && !IsConfirmationVisible;

    private bool CanRequestLegacyFlash() => SelectedImage is not null && !session.IsBusy && !IsFlashOperationActive && !IsConfirmationVisible;

    private bool CanRequestPresetFlash(QuickFlashPresetItemViewModel? item) =>
        item?.SelectedImage is not null && !session.IsBusy && !IsFlashOperationActive && !IsConfirmationVisible;

    private bool CanRequestBatchFlash() =>
        Presets.Any(item => item.HasImage) && !session.IsBusy && !IsFlashOperationActive && !IsConfirmationVisible;

    private bool CanConfirmFlash() =>
        (Presets.Any(item => item.SelectedImage is not null) || SelectedImage is not null) &&
        !IsFlashOperationActive &&
        (coordinator is null || !coordinator.IsBusy);

    private void NotifyCommandsCanExecuteChanged()
    {
        BrowseImageCommand.NotifyCanExecuteChanged();
        BrowsePresetImageCommand.NotifyCanExecuteChanged();
        RequestFlashCommand.NotifyCanExecuteChanged();
        RequestPresetFlashCommand.NotifyCanExecuteChanged();
        RequestBatchFlashCommand.NotifyCanExecuteChanged();
        ConfirmFlashCommand.NotifyCanExecuteChanged();
        CancelActiveFlashCommand.NotifyCanExecuteChanged();
    }

    private void CancelActiveFlash()
    {
        if (coordinator is not null)
        {
            coordinator.CancelCurrent();
            return;
        }

        flashCancellation?.Cancel();
    }

    private static string BuildConfirmationSummary(QuickFlashExecutionPlan plan)
    {
        var summary = $"将刷写 {plan.Requests.Count} 个分区";
        if (plan.Options.FlashBothSlots)
        {
            summary += "，双槽刷入";
        }

        if (plan.Options.SwitchSlotAfterFlash)
        {
            summary += "，刷完切换槽位";
        }

        if (!plan.Options.AutoReboot)
        {
            summary += "，不自动重启";
        }

        return summary + "。";
    }

    private static string FormatBytes(long sizeBytes)
    {
        return sizeBytes switch
        {
            < 1024 => $"{sizeBytes} B",
            < 1024 * 1024 => $"{sizeBytes / 1024d:F1} KB",
            < 1024L * 1024 * 1024 => $"{sizeBytes / 1024d / 1024:F1} MB",
            _ => $"{sizeBytes / 1024d / 1024 / 1024:F2} GB"
        };
    }

    // ---- 刷写进度(右侧 DEVICE STATUS 卡):当前分区 + 总进度 + 速度 ----

    private string? lastReportedPartition;
    private int completedFlushes;
    private int totalFlushes;
    private long lastSpeedBytes;
    private long lastSpeedTicks;

    private void ResetFlashProgress(QuickFlashExecutionPlan plan)
    {
        CurrentPartition = "--";
        CurrentPartitionProgress = 0;
        OverallProgress = 0;
        SpeedText = string.Empty;
        completedFlushes = 0;
        totalFlushes = plan.Requests.Count * (plan.Options.FlashBothSlots ? 2 : 1);
        lastReportedPartition = null;
        lastSpeedBytes = 0;
        lastSpeedTicks = 0;
    }

    private void OnFlashProgress((string Partition, double Fraction) value, QuickFlashExecutionPlan plan)
    {
        if (lastReportedPartition is not null &&
            !string.Equals(lastReportedPartition, value.Partition, StringComparison.Ordinal))
        {
            completedFlushes++;
        }

        lastReportedPartition = value.Partition;
        CurrentPartition = value.Partition;
        CurrentPartitionProgress = value.Fraction;
        OverallProgress = totalFlushes > 0
            ? (completedFlushes + Math.Clamp(value.Fraction, 0d, 1d)) / totalFlushes
            : Math.Clamp(value.Fraction, 0d, 1d);

        var sizeBytes = FindImageSize(plan, value.Partition);
        if (sizeBytes > 0)
        {
            UpdateFlashSpeed((long)(value.Fraction * sizeBytes));
        }
    }

    private static long FindImageSize(QuickFlashExecutionPlan plan, string targetPartition)
    {
        var baseName = BasePartitionName(targetPartition);
        foreach (var request in plan.Requests)
        {
            if (string.Equals(ToPartitionName(request.Partition), baseName, StringComparison.OrdinalIgnoreCase))
            {
                return request.Image.SizeBytes;
            }
        }

        return 0;
    }

    private void UpdateFlashSpeed(long bytes)
    {
        var now = Environment.TickCount64;
        if (lastSpeedBytes > 0 && now > lastSpeedTicks)
        {
            var deltaBytes = bytes - lastSpeedBytes;
            var deltaSeconds = (now - lastSpeedTicks) / 1000.0;
            if (deltaBytes > 0 && deltaSeconds > 0)
            {
                SpeedText = FormatBytes((long)(deltaBytes / deltaSeconds)) + "/s";
            }
        }

        lastSpeedBytes = bytes;
        lastSpeedTicks = now;
    }

    private static string BasePartitionName(string targetPartition) =>
        targetPartition.EndsWith("_a", StringComparison.OrdinalIgnoreCase)
            ? targetPartition[..^2]
            : targetPartition.EndsWith("_b", StringComparison.OrdinalIgnoreCase)
                ? targetPartition[..^2]
                : targetPartition;

    private static string ToPartitionName(QuickFlashPartition partition) => partition switch
    {
        QuickFlashPartition.Boot => "boot",
        QuickFlashPartition.InitBoot => "init_boot",
        QuickFlashPartition.VendorBoot => "vendor_boot",
        QuickFlashPartition.Lk => "lk",
        _ => string.Empty
    };
}
