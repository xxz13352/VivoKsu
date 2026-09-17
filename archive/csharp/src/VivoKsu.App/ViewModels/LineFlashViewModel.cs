using System.Collections.ObjectModel;
using System.IO;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public partial class LineFlashViewModel : ObservableObject
{
    private readonly DeviceSessionViewModel session;
    private readonly FastbootPartitionService partitionService;
    private readonly OperationLogService logs;
    private readonly FirmwarePackageInspector firmwarePackageInspector;
    private readonly FirmwarePackageExtractionService firmwarePackageExtractionService;
    private Action<FlashImageInfo, QuickFlashPartition>? quickFlashContinuation;

    [ObservableProperty]
    private string tableStatusText = "等待 Fastboot 设备";

    [ObservableProperty]
    private string activeSlot = "--";

    [ObservableProperty]
    private string modeLabel = "--";

    [ObservableProperty]
    private bool isRefreshing;

    [ObservableProperty]
    private bool isInspectingVivoPackage;

    [ObservableProperty]
    private FirmwarePackageInspection? vivoPackage;

    [ObservableProperty]
    private string? selectedManagedImageEntry;

    [ObservableProperty]
    private bool isExtractingImage;

    public ObservableCollection<FastbootPartitionInfo> Partitions { get; } = [];

    public IAsyncRelayCommand RefreshCommand { get; }

    public IAsyncRelayCommand BrowseVivoPackageCommand { get; }

    public IAsyncRelayCommand ExtractToQuickFlashCommand { get; }

    public LineFlashViewModel(
        DeviceSessionViewModel session,
        FastbootPartitionService partitionService,
        OperationLogService logs)
        : this(session, partitionService, logs, new FirmwarePackageInspector(), new FirmwarePackageExtractionService())
    {
    }

    public LineFlashViewModel(
        DeviceSessionViewModel session,
        FastbootPartitionService partitionService,
        OperationLogService logs,
        FirmwarePackageInspector firmwarePackageInspector,
        FirmwarePackageExtractionService firmwarePackageExtractionService)
    {
        this.session = session;
        this.partitionService = partitionService;
        this.logs = logs;
        this.firmwarePackageInspector = firmwarePackageInspector;
        this.firmwarePackageExtractionService = firmwarePackageExtractionService;
        RefreshCommand = new AsyncRelayCommand(() => RefreshAsync());
        BrowseVivoPackageCommand = new AsyncRelayCommand(BrowseVivoPackageAsync);
        ExtractToQuickFlashCommand = new AsyncRelayCommand(ExtractToQuickFlashAsync, CanExtractToQuickFlash);
        ResetTable();
    }

    public string VivoPackageName => VivoPackage?.PackageName ?? "未选择 Vivo 固件包";

    public string VivoPackageSummary => VivoPackage is null
        ? "仅检查 ZIP 内部镜像清单，不执行线刷。"
        : $"{VivoPackage.EntryCount} 个条目，识别到 {VivoPackage.ImageEntries.Count} 个镜像，其中 {VivoPackage.ManagedImageEntries.Count} 个可映射到受控分区";

    public string ManagedVivoPackageImagesText => VivoPackage is null
        ? "选择固件后显示 boot / init_boot / vendor_boot 映射"
        : VivoPackage.ManagedImageEntries.Count == 0
            ? "未识别到受控分区镜像"
            : string.Join("  ·  ", VivoPackage.ManagedImageEntries.Select(Path.GetFileName));

    partial void OnVivoPackageChanged(FirmwarePackageInspection? value)
    {
        SelectedManagedImageEntry = value?.ManagedImageEntries.FirstOrDefault();
        OnPropertyChanged(nameof(VivoPackageName));
        OnPropertyChanged(nameof(VivoPackageSummary));
        OnPropertyChanged(nameof(ManagedVivoPackageImagesText));
        ExtractToQuickFlashCommand.NotifyCanExecuteChanged();
    }

    partial void OnSelectedManagedImageEntryChanged(string? value) => ExtractToQuickFlashCommand.NotifyCanExecuteChanged();

    partial void OnIsExtractingImageChanged(bool value) => ExtractToQuickFlashCommand.NotifyCanExecuteChanged();

    public void SetQuickFlashContinuation(Action<FlashImageInfo, QuickFlashPartition> continuation)
    {
        quickFlashContinuation = continuation;
    }

    public Task RefreshAsync(bool logIfUnavailable = true) => RefreshCoreAsync(logIfUnavailable, automatic: false);

    public Task RefreshAutomaticallyAsync() => RefreshCoreAsync(logIfUnavailable: false, automatic: true);

    private async Task RefreshCoreAsync(bool logIfUnavailable, bool automatic)
    {
        if (automatic && session.IsBusy)
        {
            return;
        }

        if (session.ConnectionState != DeviceConnectionState.FastbootConnected || string.IsNullOrWhiteSpace(session.Serial) || session.Serial == "--")
        {
            ResetTable();
            if (logIfUnavailable)
            {
                logs.Write(OperationLogLevel.Warning, "请先连接 Fastboot 设备后读取分区表。");
            }

            return;
        }

        IsRefreshing = true;
        if (!automatic)
        {
            session.BeginOperation(OperationKind.Discovering, "正在读取分区表");
            logs.Write(OperationLogLevel.Info, "正在读取 Fastboot 分区表。");
        }

        try
        {
            var table = await partitionService.ReadAsync(session.Serial, CancellationToken.None);
            if (automatic && session.IsBusy)
            {
                return;
            }

            ApplyTable(table);
            if (!automatic)
            {
                session.CompleteOperation("分区表已更新");
                logs.Write(OperationLogLevel.Success, "Fastboot 分区表已更新。");
            }
        }
        catch (Exception exception)
        {
            ResetTable();
            if (!automatic)
            {
                session.FailOperation("读取分区表失败");
                logs.Write(OperationLogLevel.Error, exception.Message);
            }
        }
        finally
        {
            IsRefreshing = false;
        }
    }

    private async Task BrowseVivoPackageAsync()
    {
        var dialog = new OpenFileDialog
        {
            Filter = "Vivo 固件包 (*.zip)|*.zip",
            CheckFileExists = true,
            Multiselect = false,
            Title = "选择用于检查的 Vivo 固件包"
        };

        if (dialog.ShowDialog() != true)
        {
            return;
        }

        IsInspectingVivoPackage = true;
        logs.Write(OperationLogLevel.Info, $"正在检查 Vivo 固件包: {Path.GetFileName(dialog.FileName)}。 ");

        try
        {
            VivoPackage = await firmwarePackageInspector.InspectAsync(dialog.FileName, CancellationToken.None);
            logs.Write(OperationLogLevel.Success, $"Vivo 固件包检查完成，识别到 {VivoPackage.ImageEntries.Count} 个镜像。 ");
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
        finally
        {
            IsInspectingVivoPackage = false;
        }
    }

    private async Task ExtractToQuickFlashAsync()
    {
        if (VivoPackage is null || string.IsNullOrWhiteSpace(SelectedManagedImageEntry) || !CanExtractToQuickFlash())
        {
            return;
        }

        IsExtractingImage = true;
        session.BeginOperation(OperationKind.Hashing, "正在提取固件镜像");
        logs.Write(OperationLogLevel.Info, $"正在从固件包提取 {Path.GetFileName(SelectedManagedImageEntry)}。 ");

        try
        {
            var result = await firmwarePackageExtractionService.ExtractAsync(
                VivoPackage,
                SelectedManagedImageEntry,
                CancellationToken.None);
            session.CompleteOperation("镜像已移交快速刷写");
            logs.Write(OperationLogLevel.Success, $"{Path.GetFileName(SelectedManagedImageEntry)} 已提取并预填到快速刷写。 ");
            quickFlashContinuation?.Invoke(result.Image, result.Partition);
        }
        catch (Exception exception)
        {
            session.FailOperation("固件镜像提取失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
        finally
        {
            IsExtractingImage = false;
        }
    }

    private bool CanExtractToQuickFlash() =>
        VivoPackage is not null &&
        !string.IsNullOrWhiteSpace(SelectedManagedImageEntry) &&
        !IsExtractingImage;

    private void ApplyTable(FastbootPartitionTableSnapshot table)
    {
        ActiveSlot = table.ActiveSlot;
        ModeLabel = table.ModeLabel;
        TableStatusText = "分区信息已读取";
        ReplacePartitions(table.Partitions);
    }

    private void ResetTable()
    {
        ActiveSlot = "--";
        ModeLabel = "--";
        TableStatusText = "等待 Fastboot 设备";
        ReplacePartitions(
        [
            new FastbootPartitionInfo("boot", "启动镜像", "--", "未读取"),
            new FastbootPartitionInfo("init_boot", "GKI 初始启动镜像", "--", "未读取"),
            new FastbootPartitionInfo("vendor_boot", "Vendor Boot 镜像", "--", "未读取")
        ]);
    }

    private void ReplacePartitions(IEnumerable<FastbootPartitionInfo> values)
    {
        Partitions.Clear();
        foreach (var value in values)
        {
            Partitions.Add(value);
        }
    }
}
