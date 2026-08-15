using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Windows;
using System.Windows.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public partial class FirmwareExtractViewModel : ObservableObject
{
    private PayloadDumperRunner? payloadDumper;
    private readonly PayloadDumperProvisioner? provisioner;
    private readonly VivoFirmwareExtractor? vivoExtractor;
    private readonly OperationLogService logs;
    private Action<FlashImageInfo, QuickFlashPartition>? flashContinuation;
    private CancellationTokenSource? extractionCancellation;
    private CancellationTokenSource? readCancellation;
    private readonly Stopwatch extractionStopwatch = new();
    private DispatcherTimer? elapsedTimer;
    private string? payloadSource;
    private string? preparedGzipPath;
    private IReadOnlyList<PayloadExtractionResult> extractedImages = [];
    private long lastSpeedBytes;
    private double lastSpeedTime;
    private long payloadTotalSizeBytes;
    private long payloadWrittenBytes;
    private long lastSampledRunBytes;
    private long currentPartitionSizeBytes;

    [ObservableProperty]
    private string payloadSourceUrl = string.Empty;

    [ObservableProperty]
    private string outputPath = string.Empty;

    [ObservableProperty]
    private bool isPayloadBusy;

    [ObservableProperty]
    private double payloadProgress;

    [ObservableProperty]
    private double currentPartitionProgress;

    [ObservableProperty]
    private string currentPartitionName = "--";

    [ObservableProperty]
    private string payloadStatusText = "未加载 payload";

    [ObservableProperty]
    private string speedText = "--";

    [ObservableProperty]
    private string elapsedText = "00:00";

    [ObservableProperty]
    private bool hasExtractedImages;

    public FirmwareExtractViewModel(
        OperationLogService logs,
        PayloadDumperRunner? payloadDumper = null,
        VivoFirmwareExtractor? vivoExtractor = null,
        PayloadDumperProvisioner? provisioner = null)
    {
        this.logs = logs;
        this.payloadDumper = payloadDumper;
        this.vivoExtractor = vivoExtractor;
        this.provisioner = provisioner;

        SelectPayloadCommand = new AsyncRelayCommand(SelectPayloadAsync, CanManagePayload);
        SelectOutputPathCommand = new AsyncRelayCommand(SelectOutputPathAsync, CanManagePayload);
        ReadInfoCommand = new AsyncRelayCommand(ReadInfoAsync, CanManagePayload);
        ExtractCommand = new AsyncRelayCommand(ExtractAsync, CanExtract);
        StopCommand = new RelayCommand(Stop, () => IsPayloadBusy);
        MapToQuickFlashCommand = new RelayCommand(MapToQuickFlash, () => HasExtractedImages && flashContinuation is not null);
    }

    public IAsyncRelayCommand SelectPayloadCommand { get; }

    public IAsyncRelayCommand SelectOutputPathCommand { get; }

    public IAsyncRelayCommand ReadInfoCommand { get; }

    public IAsyncRelayCommand ExtractCommand { get; }

    public IRelayCommand StopCommand { get; }

    public IRelayCommand MapToQuickFlashCommand { get; }

    public ObservableCollection<PayloadPartitionItemViewModel> PayloadPartitions { get; } = [];

    public bool IsPayloadToolAvailable => payloadDumper?.IsAvailable == true;

    public string PayloadProgressPercent => $"{PayloadProgress:P0}";

    /// <summary>
    /// True while the tool is running but no per-partition byte progress is available yet
    /// (e.g. during the partition-list read or between partitions) — keeps the bar animated.
    /// </summary>
    public bool IsCurrentPartitionIndeterminate => IsPayloadBusy && CurrentPartitionProgress <= 0;

    partial void OnPayloadProgressChanged(double value) => OnPropertyChanged(nameof(PayloadProgressPercent));

    partial void OnCurrentPartitionProgressChanged(double value) =>
        OnPropertyChanged(nameof(IsCurrentPartitionIndeterminate));

    public void SetFlashContinuation(Action<FlashImageInfo, QuickFlashPartition> continuation)
    {
        flashContinuation = continuation;
        MapToQuickFlashCommand.NotifyCanExecuteChanged();
    }

    partial void OnPayloadSourceUrlChanged(string value)
    {
        // 固件路径一旦与已读取的源不一致,当前加载的分区/缓存全部失效。
        // 立即清空它们,防止用户在未重新读取信息时点「提取」,从旧固件的
        // 缓存 gzip 解包出旧镜像(经「映射到快速刷写」刷入后会被当成新固件)。
        if (payloadSource is not null &&
            !string.Equals(value.Trim(), payloadSource, StringComparison.OrdinalIgnoreCase))
        {
            payloadSource = null;
            // 只置空会泄漏已下载的数 GB 临时 gzip(每次换源累积);必须走带
            // 目录前缀守卫的清理器——本地源路径不会位于下载目录,不会被删。
            CleanupPreparedGzip();
            PayloadPartitions.Clear();
            extractedImages = [];
            HasExtractedImages = false;
            PayloadStatusText = "固件路径已更改，请重新读取信息。";
        }
    }

    partial void OnIsPayloadBusyChanged(bool value)
    {
        OnPropertyChanged(nameof(IsCurrentPartitionIndeterminate));
        SelectPayloadCommand.NotifyCanExecuteChanged();
        SelectOutputPathCommand.NotifyCanExecuteChanged();
        ReadInfoCommand.NotifyCanExecuteChanged();
        ExtractCommand.NotifyCanExecuteChanged();
        StopCommand.NotifyCanExecuteChanged();
        MapToQuickFlashCommand.NotifyCanExecuteChanged();
    }

    partial void OnHasExtractedImagesChanged(bool value) => MapToQuickFlashCommand.NotifyCanExecuteChanged();

    private bool CanManagePayload() => payloadDumper is not null && !IsPayloadBusy;

    private bool CanExtract() =>
        payloadDumper is not null && !IsPayloadBusy && PayloadPartitions.Any(partition => partition.IsSelected);

    /// <summary>
    /// 取得可用的 payload_dumper runner;发布版不随包,首次使用经 provisioner 按需下载。
    /// provisioner 为 null(测试)时原样返回已有 runner,保持既有注入行为。
    /// </summary>
    private async Task<PayloadDumperRunner?> AcquirePayloadDumperAsync(CancellationToken cancellationToken)
    {
        if (payloadDumper?.IsAvailable == true)
        {
            return payloadDumper;
        }

        if (provisioner is null)
        {
            return payloadDumper;
        }

        payloadDumper = await provisioner.EnsureInstalledAsync(cancellationToken);
        return payloadDumper;
    }

    private async Task SelectPayloadAsync()
    {
        var dialog = new OpenFileDialog
        {
            Filter = "Android OTA 固件 (*.bin;*.zip)|*.bin;*.zip|全部文件 (*.*)|*.*",
            CheckFileExists = true,
            Title = "选择 payload.bin 或包含 payload.bin 的 OTA 包"
        };

        if (dialog.ShowDialog() == true)
        {
            PayloadSourceUrl = dialog.FileName;
            await ReadInfoAsync();
        }
    }

    private async Task SelectOutputPathAsync()
    {
        var dialog = new OpenFolderDialog { Title = "选择镜像保存目录" };
        if (dialog.ShowDialog() == true)
        {
            OutputPath = dialog.FolderName;
        }

        await Task.CompletedTask;
    }

    private async Task ReadInfoAsync()
    {
        if (IsPayloadBusy)
        {
            return;
        }

        var source = PayloadSourceUrl.Trim();
        if (string.IsNullOrWhiteSpace(source))
        {
            PayloadStatusText = "请输入本地固件路径或云端直链。";
            return;
        }

        if (!Uri.TryCreate(source, UriKind.Absolute, out var uri) ||
            (uri.Scheme is not ("http" or "https") && !File.Exists(source)))
        {
            PayloadStatusText = "固件路径无效:需要本地文件或 http/https 直链。";
            return;
        }

        payloadSource = source;
        CleanupPreparedGzip();
        IsPayloadBusy = true;
        readCancellation = new CancellationTokenSource();
        extractionStopwatch.Restart();
        lastSpeedBytes = 0;
        lastSpeedTime = 0;
        StartElapsedTicker();
        PayloadProgress = 0;
        CurrentPartitionName = "--";
        PayloadStatusText = "正在识别固件格式…";
        try
        {
            var kind = await FirmwareFormatDetector.DetectAsync(source, readCancellation.Token);
            if (kind == FirmwareFormatDetector.FirmwareKind.VivoGzip)
            {
                if (vivoExtractor is null)
                {
                    PayloadStatusText = "Vivo 固件提取器未就绪。";
                    return;
                }

                await ReadVivoInfoAsync(source, readCancellation.Token);
            }
            else if (kind == FirmwareFormatDetector.FirmwareKind.PayloadZip)
            {
                if (await AcquirePayloadDumperAsync(readCancellation.Token) is not { } dumper)
                {
                    PayloadStatusText = "payload 提取器未就绪。";
                    return;
                }

                PayloadStatusText = "正在读取 payload 分区列表…";
                var partitions = await dumper.ListPartitionsAsync(source, readCancellation.Token);
                PopulatePartitions(partitions.Select(partition => new PayloadPartitionItemViewModel(partition)));
            }
            else
            {
                PayloadStatusText = "无法识别的固件格式（支持 payload.bin / OTA zip / Vivo gzip 固件）。";
            }
        }
        catch (OperationCanceledException)
        {
            PayloadStatusText = "已停止。";
        }
        catch (Exception exception)
        {
            PayloadStatusText = $"读取失败: {exception.Message}";
            logs.Write(OperationLogLevel.Error, $"读取固件失败: {exception.Message}");
        }
        finally
        {
            StopElapsedTicker();
            IsPayloadBusy = false;
            readCancellation?.Dispose();
            readCancellation = null;
        }
    }

    private async Task ReadVivoInfoAsync(string source, CancellationToken cancellationToken)
    {
        PayloadStatusText = "正在下载 Vivo 固件…";
        CurrentPartitionName = "下载固件";
        var gzipPath = await vivoExtractor!.PrepareGzipAsync(source, new Progress<VivoFirmwareExtractor.VivoProgress>(OnVivoProgress), cancellationToken);
        preparedGzipPath = gzipPath;
        PayloadStatusText = "正在读取分区列表…";
        CurrentPartitionName = "解析固件";
        var entries = await vivoExtractor.ListAsync(gzipPath, new Progress<VivoFirmwareExtractor.VivoProgress>(OnVivoProgress), cancellationToken);
        PopulatePartitions(entries.Select(entry => new PayloadPartitionItemViewModel(entry.Name, entry.SizeBytes, entry.FullPath)));
        CurrentPartitionName = "--";
    }

    private void PopulatePartitions(IEnumerable<PayloadPartitionItemViewModel> items)
    {
        PayloadPartitions.Clear();
        foreach (var item in items.OrderBy(item => item.Name, StringComparer.OrdinalIgnoreCase))
        {
            item.PropertyChanged += OnPayloadPartitionPropertyChanged;
            PayloadPartitions.Add(item);
        }

        PayloadStatusText = PayloadPartitions.Count == 0
            ? "未在固件中找到可提取的分区。"
            : $"已读取 {PayloadPartitions.Count} 个分区，勾选后提取镜像。";
    }

    private void CleanupPreparedGzip()
    {
        if (preparedGzipPath is null)
        {
            return;
        }

        // 只删除应用自己下载到临时目录的 gzip。本地源 PrepareGzipAsync 原样返回
        // 用户自己的固件路径,绝不能 File.Delete——否则第二次读取信息会静默销毁
        // 用户的本地固件文件。
        if (!preparedGzipPath.StartsWith(
                VivoFirmwareExtractor.DownloadedGzipDirectory, StringComparison.OrdinalIgnoreCase))
        {
            preparedGzipPath = null;
            return;
        }

        try
        {
            File.Delete(preparedGzipPath);
        }
        catch
        {
            // Best effort.
        }

        preparedGzipPath = null;
    }

    private void OnVivoProgress(VivoFirmwareExtractor.VivoProgress progress)
    {
        PayloadProgress = progress.Fraction;
        CurrentPartitionProgress = progress.Fraction;
        if (progress.CurrentEntry is not null)
        {
            CurrentPartitionName = progress.CurrentEntry;
        }

        UpdateElapsed();
        var now = extractionStopwatch.Elapsed.TotalSeconds;
        if (now - lastSpeedTime >= 0.5)
        {
            var deltaBytes = progress.ProcessedBytes - lastSpeedBytes;
            var deltaTime = now - lastSpeedTime;
            if (deltaBytes > 0 && deltaTime > 0)
            {
                SpeedText = FormatBytes((long)(deltaBytes / deltaTime)) + "/s";
            }

            lastSpeedBytes = progress.ProcessedBytes;
            lastSpeedTime = now;
        }
    }

    /// <summary>
    /// Called from a Progress&lt;long&gt; marshaled onto the UI thread while payload_dumper runs.
    /// <paramref name="bytes"/> is the process's cumulative bytes written for the current
    /// partition run (the write counter; payload_dumper's network reads don't show up as read
    /// I/O but the output file writes do). The delta is accumulated across runs to drive the
    /// total bar against the total selected size, and the raw value drives the current bar.
    /// </summary>
    private void OnPayloadWriteProgress(long bytes)
    {
        payloadWrittenBytes += bytes - lastSampledRunBytes;
        lastSampledRunBytes = bytes;

        if (payloadTotalSizeBytes > 0)
        {
            PayloadProgress = Math.Min(payloadWrittenBytes / (double)payloadTotalSizeBytes, 1);
        }

        if (currentPartitionSizeBytes > 0)
        {
            CurrentPartitionProgress = Math.Min(bytes / (double)currentPartitionSizeBytes, 1);
        }

        UpdateElapsed();
        var now = extractionStopwatch.Elapsed.TotalSeconds;
        if (now - lastSpeedTime >= 0.3)
        {
            var deltaBytes = payloadWrittenBytes - lastSpeedBytes;
            var deltaTime = now - lastSpeedTime;
            if (deltaBytes > 0 && deltaTime > 0)
            {
                SpeedText = FormatBytes((long)(deltaBytes / deltaTime)) + "/s";
            }

            lastSpeedBytes = payloadWrittenBytes;
            lastSpeedTime = now;
        }
    }


    private async Task ExtractAsync()
    {
        if (IsPayloadBusy || payloadSource is null)
        {
            return;
        }

        // 固件路径与已读取的源不一致(改了路径文本框但没重新读取信息):
        // 拒绝从旧固件的缓存 gzip 解包,防止解出旧镜像被当成新固件刷入。
        if (!string.Equals(PayloadSourceUrl.Trim(), payloadSource, StringComparison.OrdinalIgnoreCase))
        {
            PayloadStatusText = "固件路径已更改，请先重新读取信息再提取。";
            return;
        }

        var selected = PayloadPartitions.Where(partition => partition.IsSelected).ToArray();
        if (selected.Length == 0)
        {
            return;
        }

        var outputDirectory = string.IsNullOrWhiteSpace(OutputPath)
            ? Path.Combine(Path.GetTempPath(), "VivoKsu", "payload-extract", Guid.NewGuid().ToString("N"))
            : OutputPath;

        extractionCancellation = new CancellationTokenSource();
        extractionStopwatch.Restart();
        lastSpeedBytes = 0;
        lastSpeedTime = 0;
        StartElapsedTicker();
        IsPayloadBusy = true;
        PayloadProgress = 0;
        CurrentPartitionName = "--";
        SpeedText = "--";
        // 新一轮提取开始即作废上一轮的提取结果:若本次失败/取消,绝不能把上一轮
        // 镜像当作本次结果经「映射到快速刷写」送进刷写页。
        extractedImages = [];
        HasExtractedImages = false;
        var extracted = new List<PayloadExtractionResult>();
        try
        {
            var kind = await FirmwareFormatDetector.DetectAsync(payloadSource, extractionCancellation.Token);
            if (kind == FirmwareFormatDetector.FirmwareKind.VivoGzip)
            {
                if (vivoExtractor is null || preparedGzipPath is null)
                {
                    PayloadStatusText = "Vivo 固件提取器未就绪。";
                    return;
                }

                // Single streaming pass over the cached gzip: real continuous progress.
                var targets = selected.Select(partition => new VivoFirmwareExtractor.VivoFirmwareEntry(
                    partition.Name, partition.FullPath, partition.SizeBytes)).ToArray();
                PayloadStatusText = $"正在提取 {selected.Length} 个镜像…";
                var results = await vivoExtractor.ExtractAsync(
                    preparedGzipPath, targets, outputDirectory, new Progress<VivoFirmwareExtractor.VivoProgress>(OnVivoProgress), extractionCancellation.Token);
                extracted.AddRange(results.Select(result =>
                    new PayloadExtractionResult(result.EntryName, result.OutputPath, result.SizeBytes)));
            }
            else
            {
                if (await AcquirePayloadDumperAsync(extractionCancellation.Token) is not { } dumper)
                {
                    PayloadStatusText = "payload 提取器未就绪。";
                    return;
                }

                // payload_dumper reads only the needed blobs from the URL via Range requests,
                // so the source stays remote. It never streams progress and its network reads
                // don't register as process read I/O, but it DOES write the partition images
                // incrementally — so we sample the process's bytes-written counter and report it
                // against the partition's raw size for real, continuous progress and speed.
                payloadTotalSizeBytes = selected.Sum(partition => partition.SizeBytes);
                payloadWrittenBytes = 0;
                lastSampledRunBytes = 0;
                for (var index = 0; index < selected.Length; index++)
                {
                    extractionCancellation.Token.ThrowIfCancellationRequested();
                    var name = selected[index].Name;
                    CurrentPartitionName = name;
                    CurrentPartitionProgress = 0;
                    currentPartitionSizeBytes = selected[index].SizeBytes;
                    lastSampledRunBytes = 0;
                    PayloadStatusText = $"正在提取 {name}（{index + 1}/{selected.Length}）…";
                    var progress = new Progress<long>(OnPayloadWriteProgress);
                    var results = await dumper.ExtractAsync(
                        payloadSource, [name], outputDirectory, extractionCancellation.Token, progress);
                    extracted.AddRange(results);

                    // Make sure the current partition shows complete and the total advances,
                    // even if no sample arrived (tiny local payloads finish between polls).
                    CurrentPartitionProgress = 1;
                    if (payloadTotalSizeBytes > 0)
                    {
                        PayloadProgress = Math.Min(payloadWrittenBytes / (double)payloadTotalSizeBytes, 1);
                    }
                    else
                    {
                        PayloadProgress = (index + 1d) / selected.Length;
                    }

                    UpdateElapsed();
                    if (SpeedText == "--")
                    {
                        SpeedText = FormatBytes(ComputeSpeed(extracted)) + "/s";
                    }
                }
            }

            extractionStopwatch.Stop();
            UpdateElapsed();
            var finalSeconds = Math.Max(extractionStopwatch.Elapsed.TotalSeconds, 0.001);
            SpeedText = FormatBytes((long)(extracted.Sum(result => result.SizeBytes) / finalSeconds)) + "/s";
            CurrentPartitionName = "--";
            CurrentPartitionProgress = 0;
            extractedImages = extracted;
            HasExtractedImages = extracted.Count > 0;
            PayloadProgress = 1;
            PayloadStatusText = extracted.Count == 0
                ? "未生成任何镜像。"
                : $"已提取 {extracted.Count} 个镜像到 {outputDirectory}";
            logs.Write(OperationLogLevel.Success, $"已提取 {extracted.Count} 个镜像到 {outputDirectory}。");
        }
        catch (OperationCanceledException)
        {
            PayloadStatusText = "提取已停止。";
        }
        catch (Exception exception)
        {
            PayloadStatusText = $"提取失败: {exception.Message}";
            logs.Write(OperationLogLevel.Error, $"提取固件失败: {exception.Message}");
        }
        finally
        {
            StopElapsedTicker();
            IsPayloadBusy = false;
            CurrentPartitionName = "--";
            extractionCancellation?.Dispose();
            extractionCancellation = null;
        }
    }

    private void Stop()
    {
        // 读取信息/下载 Vivo 固件期间用 readCancellation,提取期间用 extractionCancellation;
        // 两者互斥(IsPayloadBusy 门),取消当前激活的那个。
        readCancellation?.Cancel();
        extractionCancellation?.Cancel();
    }

    private void MapToQuickFlash()
    {
        if (flashContinuation is null)
        {
            return;
        }

        var mapped = 0;
        foreach (var result in extractedImages)
        {
            var partition = ToQuickFlashPartition(result.PartitionName);
            if (partition is not null)
            {
                flashContinuation(new FlashImageInfo(result.OutputPath, result.SizeBytes), partition.Value);
                mapped++;
            }
        }

        if (mapped > 0)
        {
            PayloadStatusText = $"{mapped} 个镜像已映射到快速刷写。";
            logs.Write(OperationLogLevel.Success, $"{mapped} 个镜像已映射到快速刷写。");
        }
        else
        {
            PayloadStatusText = "提取的镜像未匹配快速刷写预设（boot/init_boot/vendor_boot/lk）。";
        }
    }

    private void OnPayloadPartitionPropertyChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName == nameof(PayloadPartitionItemViewModel.IsSelected))
        {
            ExtractCommand.NotifyCanExecuteChanged();
        }
    }

    private static QuickFlashPartition? ToQuickFlashPartition(string partitionName) => partitionName.ToLowerInvariant() switch
    {
        "boot" => QuickFlashPartition.Boot,
        "init_boot" => QuickFlashPartition.InitBoot,
        "vendor_boot" => QuickFlashPartition.VendorBoot,
        "lk" => QuickFlashPartition.Lk,
        _ => null
    };

    private long ComputeSpeed(IReadOnlyList<PayloadExtractionResult> results)
    {
        if (results.Count == 0)
        {
            return 0;
        }

        var seconds = Math.Max(extractionStopwatch.Elapsed.TotalSeconds, 0.001);
        return (long)(results.Sum(result => result.SizeBytes) / seconds);
    }

    private void StartElapsedTicker()
    {
        if (Application.Current is null)
        {
            return;
        }

        elapsedTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(500) };
        elapsedTimer.Tick += (_, _) => UpdateElapsed();
        elapsedTimer.Start();
    }

    private void StopElapsedTicker()
    {
        elapsedTimer?.Stop();
        elapsedTimer = null;
    }

    private void UpdateElapsed()
    {
        if (extractionStopwatch.ElapsedTicks > 0)
        {
            ElapsedText = extractionStopwatch.Elapsed.ToString(@"hh\:mm\:ss");
        }
    }

    private static string FormatBytes(long bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{bytes / 1024d / 1024:F1} MB",
        _ => $"{bytes / 1024d / 1024 / 1024:F2} GB"
    };
}
