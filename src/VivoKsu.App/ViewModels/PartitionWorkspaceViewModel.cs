using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Windows;
using System.Windows.Data;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public partial class PartitionWorkspaceViewModel : ObservableObject
{
    private readonly DeviceSessionViewModel session;
    private readonly IPartitionTransport fastbootTransport;
    private readonly IPartitionTransport adbRootTransport;
    private readonly PartitionExecutionService executor;
    private readonly OperationLogService logs;
    private readonly IOperationCoordinator coordinator;
    private readonly Func<string, bool> confirm;
    private readonly PartitionExecutionPlanBuilder planBuilder = new();
    private readonly Stopwatch operationStopwatch = new();
    private PartitionSnapshot? snapshot;
    private string currentOperationLabel = "执行";

    [ObservableProperty]
    private PartitionTransportKind selectedTransport = PartitionTransportKind.Automatic;

    [ObservableProperty]
    private string filterText = string.Empty;

    [ObservableProperty]
    private string tableStatusText = "等待设备连接";

    [ObservableProperty]
    private string activeSlot = "--";

    [ObservableProperty]
    private string activeTransportText = "自动";

    [ObservableProperty]
    private bool isRefreshing;

    [ObservableProperty]
    private bool isExecuting;

    [ObservableProperty]
    private double overallProgress;

    [ObservableProperty]
    private string progressText = "等待任务";

    [ObservableProperty]
    private string currentOperationPartitionName = "--";

    [ObservableProperty]
    private double currentOperationProgress;

    [ObservableProperty]
    private string operationSpeedText = "--";

    [ObservableProperty]
    private string operationElapsedText = "00:00";

    public PartitionWorkspaceViewModel(
        DeviceSessionViewModel session,
        IPartitionTransport fastbootTransport,
        IPartitionTransport adbRootTransport,
        PartitionExecutionService executor,
        OperationLogService logs,
        IOperationCoordinator coordinator,
        Func<string, bool>? confirm = null)
    {
        this.session = session;
        this.fastbootTransport = fastbootTransport;
        this.adbRootTransport = adbRootTransport;
        this.executor = executor;
        this.logs = logs;
        this.coordinator = coordinator;
        this.confirm = confirm ?? (_ => true);

        RowsView = CollectionViewSource.GetDefaultView(Rows);
        RowsView.Filter = MatchesFilter;
        SelectAutomaticTransportCommand = new RelayCommand(() => SelectedTransport = PartitionTransportKind.Automatic);
        SelectAdbRootTransportCommand = new RelayCommand(() => SelectedTransport = PartitionTransportKind.AdbRoot);
        SelectFastbootTransportCommand = new RelayCommand(() => SelectedTransport = PartitionTransportKind.Fastboot);
        RefreshCommand = new AsyncRelayCommand(() => RefreshAsync(logIfUnavailable: true), CanRefresh);
        BrowseImagesCommand = new AsyncRelayCommand(BrowseImagesAsync, CanEditImages);
        BrowseRowImageCommand = new AsyncRelayCommand<PartitionRowViewModel?>(BrowseRowImageAsync, CanEditRowImage);
        BackupSelectedCommand = new AsyncRelayCommand(BackupSelectedAsync, CanOperateOnSelection);
        WriteSelectedCommand = new AsyncRelayCommand(WriteSelectedAsync, CanWriteSelection);
        EraseSelectedCommand = new AsyncRelayCommand(EraseSelectedAsync, CanOperateOnSelection);
        StopCommand = new RelayCommand(Stop, () => IsExecuting);

        // Commands gate on coordinator.IsBusy; re-evaluate them whenever the
        // coordinator's busy state changes (including operations from other pages).
        coordinator.StateChanged += OnCoordinatorStateChanged;
    }

    public ObservableCollection<PartitionRowViewModel> Rows { get; } = [];

    public ICollectionView RowsView { get; }

    public IRelayCommand SelectAutomaticTransportCommand { get; }

    public IRelayCommand SelectAdbRootTransportCommand { get; }

    public IRelayCommand SelectFastbootTransportCommand { get; }

    public IAsyncRelayCommand RefreshCommand { get; }

    public IAsyncRelayCommand BrowseImagesCommand { get; }

    public IAsyncRelayCommand<PartitionRowViewModel?> BrowseRowImageCommand { get; }

    public IAsyncRelayCommand BackupSelectedCommand { get; }

    public IAsyncRelayCommand WriteSelectedCommand { get; }

    public IAsyncRelayCommand EraseSelectedCommand { get; }

    public IRelayCommand StopCommand { get; }

    public int SelectedCount => Rows.Count(row => row.IsSelected);

    public int MappedImageCount => Rows.Count(row => row.HasImage);

    public string SummaryText => $"{Rows.Count} 个分区  ·  已选 {SelectedCount}  ·  已映射 {MappedImageCount}";

    public string OperationProgressPercent => $"{OverallProgress:P0}";

    /// <summary>
    /// True while an operation runs but the current partition has no byte progress yet
    /// (e.g. an erase, or before the first progress report) — keeps the bar animated.
    /// </summary>
    public bool IsCurrentOperationIndeterminate => IsExecuting && CurrentOperationProgress <= 0;

    public Task RefreshAsync(bool logIfUnavailable) => RefreshCoreAsync(logIfUnavailable);

    public void MapImages(IEnumerable<FlashImageInfo> images)
    {
        ArgumentNullException.ThrowIfNull(images);

        foreach (var image in images)
        {
            var baseName = Path.GetFileNameWithoutExtension(image.Path);
            if (string.IsNullOrWhiteSpace(baseName))
            {
                continue;
            }

            var row = Rows.FirstOrDefault(candidate => string.Equals(candidate.Name, baseName, StringComparison.OrdinalIgnoreCase));
            row ??= ActiveSlot is "a" or "b"
                ? Rows.FirstOrDefault(candidate => string.Equals(candidate.Name, $"{baseName}_{ActiveSlot}", StringComparison.OrdinalIgnoreCase))
                : null;

            if (row is not null)
            {
                row.ImagePath = image.Path;
            }
        }

        NotifySelectionChanged();
    }

    partial void OnFilterTextChanged(string value) => RowsView.Refresh();

    partial void OnSelectedTransportChanged(PartitionTransportKind value)
    {
        ActiveTransportText = GetTransportLabel(value);
    }

    partial void OnIsRefreshingChanged(bool value) => RefreshCommand.NotifyCanExecuteChanged();

    partial void OnOverallProgressChanged(double value) => OnPropertyChanged(nameof(OperationProgressPercent));

    partial void OnCurrentOperationProgressChanged(double value) => OnPropertyChanged(nameof(IsCurrentOperationIndeterminate));

    partial void OnIsExecutingChanged(bool value)
    {
        OnPropertyChanged(nameof(IsCurrentOperationIndeterminate));
        RefreshCommand.NotifyCanExecuteChanged();
        BrowseImagesCommand.NotifyCanExecuteChanged();
        BrowseRowImageCommand.NotifyCanExecuteChanged();
        BackupSelectedCommand.NotifyCanExecuteChanged();
        WriteSelectedCommand.NotifyCanExecuteChanged();
        EraseSelectedCommand.NotifyCanExecuteChanged();
        StopCommand.NotifyCanExecuteChanged();
    }

    private void OnCoordinatorStateChanged(object? sender, EventArgs eventArgs)
    {
        RefreshCommand.NotifyCanExecuteChanged();
        BrowseImagesCommand.NotifyCanExecuteChanged();
        BrowseRowImageCommand.NotifyCanExecuteChanged();
        BackupSelectedCommand.NotifyCanExecuteChanged();
        WriteSelectedCommand.NotifyCanExecuteChanged();
        EraseSelectedCommand.NotifyCanExecuteChanged();
        StopCommand.NotifyCanExecuteChanged();
    }

    // 分区表只在用户点击「读取分区表」时读取;设备心跳、补偿刷新都不会触发重读。
    private async Task RefreshCoreAsync(bool logIfUnavailable)
    {
        if (IsRefreshing || IsExecuting || coordinator.IsBusy)
        {
            return;
        }

        var transport = ResolveTransport();
        if (transport is null || string.IsNullOrWhiteSpace(session.Serial) || session.Serial == "--")
        {
            TableStatusText = "等待匹配的设备连接";
            if (logIfUnavailable)
            {
                logs.Write(OperationLogLevel.Warning, "请连接符合当前通道的 ADB Root 或 Fastboot 设备后读取分区表。 ");
            }

            return;
        }

        IsRefreshing = true;
        logs.Write(OperationLogLevel.Info, $"正在通过 {GetTransportLabel(transport.Kind)} 读取分区表。 ");

        try
        {
            var discovered = await transport.DiscoverAsync(session.Serial, CancellationToken.None);
            ApplySnapshot(discovered);
            logs.Write(OperationLogLevel.Success, $"分区表已更新，共 {Rows.Count} 个分区。 ");
        }
        catch (Exception exception)
        {
            TableStatusText = Rows.Count == 0 ? "分区表读取失败" : "保留上次读取的分区表";
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
        finally
        {
            IsRefreshing = false;
        }
    }

    private void ApplySnapshot(PartitionSnapshot value)
    {
        snapshot = value;
        ActiveSlot = string.IsNullOrWhiteSpace(value.ActiveSlot) ? "--" : value.ActiveSlot;
        ActiveTransportText = GetTransportLabel(value.Transport);
        TableStatusText = $"已读取 {value.Partitions.Count} 个分区";

        // Automatic refresh recreates every row, so carry over the user's
        // checkbox selections and image mappings by partition name.
        var previouslySelected = new HashSet<string>(
            Rows.Where(row => row.IsSelected).Select(row => row.Name),
            StringComparer.OrdinalIgnoreCase);
        var previousMappings = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (var row in Rows)
        {
            if (row.HasImage && row.ImagePath is not null)
            {
                previousMappings[row.Name] = row.ImagePath;
            }
        }

        foreach (var row in Rows)
        {
            row.PropertyChanged -= OnRowPropertyChanged;
        }

        Rows.Clear();
        foreach (var partition in value.Partitions)
        {
            var row = new PartitionRowViewModel(partition);
            row.IsSelected = previouslySelected.Contains(row.Name);
            if (previousMappings.TryGetValue(row.Name, out var imagePath))
            {
                row.ImagePath = imagePath;
            }

            row.PropertyChanged += OnRowPropertyChanged;
            Rows.Add(row);
        }

        RowsView.Refresh();
        NotifySelectionChanged();
    }

    private async Task BrowseImagesAsync()
    {
        var dialog = new OpenFileDialog
        {
            Filter = "Android 镜像 (*.img;*.bin)|*.img;*.bin",
            CheckFileExists = true,
            Multiselect = true,
            Title = "选择镜像文件"
        };

        if (dialog.ShowDialog() != true)
        {
            return;
        }

        var images = dialog.FileNames.Select(path => new FlashImageInfo(path, new FileInfo(path).Length));
        MapImages(images);
        logs.Write(OperationLogLevel.Info, $"已载入 {dialog.FileNames.Length} 个镜像并按分区名映射。 ");
    }

    private async Task BrowseRowImageAsync(PartitionRowViewModel? row)
    {
        if (row is null)
        {
            return;
        }

        var dialog = new OpenFileDialog
        {
            Filter = "Android 镜像 (*.img;*.bin)|*.img;*.bin",
            CheckFileExists = true,
            Multiselect = false,
            Title = $"选择写入 {row.Name} 的镜像"
        };

        if (dialog.ShowDialog() == true)
        {
            row.ImagePath = dialog.FileName;
        }

        await Task.CompletedTask;
    }

    private async Task BackupSelectedAsync()
    {
        var dialog = new OpenFolderDialog { Title = "选择分区备份目录" };
        if (dialog.ShowDialog() != true)
        {
            return;
        }

        await ExecuteAsync(PartitionOperationKind.Backup, dialog.FolderName);
    }

    private Task WriteSelectedAsync() => ExecuteAsync(PartitionOperationKind.Write, null);

    private Task EraseSelectedAsync() => ExecuteAsync(PartitionOperationKind.Erase, null);

    private async Task ExecuteAsync(PartitionOperationKind operation, string? outputDirectory)
    {
        if (snapshot is null)
        {
            return;
        }

        var selectedRows = Rows.Where(row => row.IsSelected).ToArray();
        var selectedPartitions = selectedRows.Select(row => row.Partition).ToArray();
        PartitionExecutionPlan plan;
        try
        {
            plan = operation switch
            {
                PartitionOperationKind.Backup => planBuilder.BuildBackup(snapshot.Serial, snapshot.Transport, selectedPartitions, outputDirectory!),
                PartitionOperationKind.Write => planBuilder.BuildWrite(
                    snapshot.Serial,
                    snapshot.Transport,
                    selectedPartitions,
                    selectedRows.Where(row => row.HasImage).ToDictionary(row => row.Name, row => row.ImagePath!, StringComparer.OrdinalIgnoreCase)),
                PartitionOperationKind.Erase => planBuilder.BuildErase(snapshot.Serial, snapshot.Transport, selectedPartitions),
                _ => throw new InvalidOperationException("未知分区操作。")
            };
        }
        catch (Exception exception)
        {
            logs.Write(OperationLogLevel.Warning, exception.Message);
            return;
        }

        var summary = BuildConfirmationText(plan, selectedRows);
        if (!confirm(summary))
        {
            return;
        }

        currentOperationLabel = operation switch
        {
            PartitionOperationKind.Backup => "备份",
            PartitionOperationKind.Write => "写入",
            PartitionOperationKind.Erase => "擦除",
            _ => "执行"
        };
        foreach (var row in selectedRows)
        {
            row.ResetTransferState();
        }

        IsExecuting = true;
        OverallProgress = 0;
        ProgressText = "正在准备任务";
        CurrentOperationPartitionName = "--";
        CurrentOperationProgress = 0;
        OperationSpeedText = "--";
        OperationElapsedText = "00:00";
        operationStopwatch.Restart();

        try
        {
            await executor.ExecuteAsync(plan, SetRowState, SetRowProgress, CancellationToken.None);
            ProgressText = $"已完成 {plan.Tasks.Count} 个分区";
        }
        catch (OperationCanceledException)
        {
            ProgressText = "任务已停止";
        }
        catch
        {
            ProgressText = "任务失败，已停止后续分区";
        }
        finally
        {
            operationStopwatch.Stop();
            IsExecuting = false;
            CurrentOperationPartitionName = "--";
            CurrentOperationProgress = 0;
        }
    }

    private void SetRowState(string name, PartitionTaskState state)
    {
        RunOnUiThread(() =>
        {
            var row = Rows.FirstOrDefault(candidate => string.Equals(candidate.Name, name, StringComparison.OrdinalIgnoreCase));
            if (row is not null)
            {
                row.TaskState = state;
            }

            if (state == PartitionTaskState.Running)
            {
                CurrentOperationPartitionName = name;
                CurrentOperationProgress = 0;
                ProgressText = $"正在{currentOperationLabel} {name}…";
            }
        });
    }

    private void SetRowProgress(PartitionTransferProgress progress)
    {
        RunOnUiThread(() =>
        {
            var row = Rows.FirstOrDefault(candidate => string.Equals(candidate.Name, progress.PartitionName, StringComparison.OrdinalIgnoreCase));
            row?.ApplyProgress(progress);
            OverallProgress = Rows.Where(candidate => candidate.IsSelected).DefaultIfEmpty().Average(candidate => candidate?.Progress ?? 0);
            if (row is not null)
            {
                CurrentOperationPartitionName = progress.PartitionName;
                CurrentOperationProgress = row.Progress;
                ProgressText = $"{row.Name}  ·  {row.TransferText}";
            }

            OperationElapsedText = operationStopwatch.Elapsed.ToString(@"hh\:mm\:ss");
            if (progress.BytesPerSecond > 0)
            {
                OperationSpeedText = FormatBytes((long)progress.BytesPerSecond) + "/s";
            }
        });
    }

    private void Stop() => coordinator.CancelCurrent();

    private IPartitionTransport? ResolveTransport()
    {
        return SelectedTransport switch
        {
            PartitionTransportKind.Fastboot when session.ConnectionState == DeviceConnectionState.FastbootConnected => fastbootTransport,
            PartitionTransportKind.AdbRoot when session.ConnectionState == DeviceConnectionState.AdbConnected => adbRootTransport,
            PartitionTransportKind.Automatic when session.ConnectionState == DeviceConnectionState.FastbootConnected => fastbootTransport,
            PartitionTransportKind.Automatic when session.ConnectionState == DeviceConnectionState.AdbConnected => adbRootTransport,
            _ => null
        };
    }

    private bool MatchesFilter(object value)
    {
        if (value is not PartitionRowViewModel row || string.IsNullOrWhiteSpace(FilterText))
        {
            return true;
        }

        return row.Name.Contains(FilterText, StringComparison.OrdinalIgnoreCase);
    }

    private bool CanRefresh() => !IsRefreshing && !IsExecuting && !coordinator.IsBusy;

    private bool CanEditImages() => !IsExecuting && !coordinator.IsBusy;

    private bool CanEditRowImage(PartitionRowViewModel? row) => row is not null && CanEditImages();

    private bool CanOperateOnSelection() => !IsExecuting && !coordinator.IsBusy && snapshot is not null && SelectedCount > 0;

    private bool CanWriteSelection() => CanOperateOnSelection() && Rows.Any(row => row.IsSelected && row.HasImage);

    private void OnRowPropertyChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName is nameof(PartitionRowViewModel.IsSelected) or nameof(PartitionRowViewModel.ImagePath))
        {
            NotifySelectionChanged();
        }
    }

    private void NotifySelectionChanged()
    {
        OnPropertyChanged(nameof(SelectedCount));
        OnPropertyChanged(nameof(MappedImageCount));
        OnPropertyChanged(nameof(SummaryText));
        BackupSelectedCommand.NotifyCanExecuteChanged();
        WriteSelectedCommand.NotifyCanExecuteChanged();
        EraseSelectedCommand.NotifyCanExecuteChanged();
    }

    private static string BuildConfirmationText(PartitionExecutionPlan plan, IReadOnlyCollection<PartitionRowViewModel> rows)
    {
        var operation = plan.Operation switch
        {
            PartitionOperationKind.Backup => "备份",
            PartitionOperationKind.Write => "写入",
            PartitionOperationKind.Erase => "擦除",
            _ => "执行"
        };
        var flagged = rows.Count(row => row.IsHighRisk || row.IsMounted);
        return flagged > 0
            ? $"将通过 {GetTransportLabel(plan.Transport)} {operation} {plan.Tasks.Count} 个分区，其中 {flagged} 个标记为高风险或已挂载。"
            : $"将通过 {GetTransportLabel(plan.Transport)} {operation} {plan.Tasks.Count} 个分区。";
    }

    private static string GetTransportLabel(PartitionTransportKind transport) => transport switch
    {
        PartitionTransportKind.AdbRoot => "ADB Root",
        PartitionTransportKind.Fastboot => "Fastboot",
        _ => "自动"
    };

    private static string FormatBytes(long bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{bytes / 1024d / 1024:F1} MB",
        _ => $"{bytes / 1024d / 1024 / 1024:F2} GB"
    };

    private static void RunOnUiThread(Action action)
    {
        var dispatcher = Application.Current?.Dispatcher;
        if (dispatcher is not null && !dispatcher.CheckAccess())
        {
            dispatcher.Invoke(action);
            return;
        }

        action();
    }
}
