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
    private readonly PayloadDumperRunner? payloadDumper;
    private readonly VivoFirmwareExtractor? vivoExtractor;
    private readonly OperationLogService logs;
    private Action<FlashImageInfo, QuickFlashPartition>? flashContinuation;
    private CancellationTokenSource? extractionCancellation;
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
        VivoFirmwareExtractor? vivoExtractor = null)
    {
        this.logs = logs;
        this.payloadDumper = payloadDumper;
        this.vivoExtractor = vivoExtractor;

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
        extractionStopwatch.Restart();
        lastSpeedBytes = 0;
        lastSpeedTime = 0;
        StartElapsedTicker();
        PayloadProgress = 0;
        CurrentPartitionName = "--";
        PayloadStatusText = "正在识别固件格式…";
        try
        {
            var kind = await FirmwareFormatDetector.DetectAsync(source, CancellationToken.None);
            if (kind == FirmwareFormatDetector.FirmwareKind.VivoGzip)
            {
                if (vivoExtractor is null)
                {
                    PayloadStatusText = "Vivo 固件提取器未就绪。";
                    return;
                }

                await ReadVivoInfoAsync(source);
            }
            else if (kind == FirmwareFormatDetector.FirmwareKind.PayloadZip)
            {
                if (payloadDumper is null)
                {
                    PayloadStatusText = "payload 提取器未就绪。";
                    return;
                }

                PayloadStatusText = "正在读取 payload 分区列表…";
                var partitions = await payloadDumper.ListPartitionsAsync(source, CancellationToken.None);
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
        }
    }

    private async Task ReadVivoInfoAsync(string source)
    {
        PayloadStatusText = "正在下载 Vivo 固件…";
        CurrentPartitionName = "下载固件";
        var gzipPath = await vivoExtractor!.PrepareGzipAsync(source, new Progress<VivoFirmwareExtractor.VivoProgress>(OnVivoProgress), CancellationToken.None);
        preparedGzipPath = gzipPath;
        PayloadStatusText = "正在读取分区列表…";
        CurrentPartitionName = "解析固件";
        var entries = await vivoExtractor.ListAsync(gzipPath, new Progress<VivoFirmwareExtractor.VivoProgress>(OnVivoProgress), CancellationToken.None);
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
        if (preparedGzipPath is not null)
        {
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
        var extracted = new List<PayloadExtractionResult>();
        try
        {
            var kind = await FirmwareFormatDetector.DetectAsync(payloadSource, CancellationToken.None);
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
                if (payloadDumper is null)
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
                    var results = await payloadDumper.ExtractAsync(
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

    private void Stop() => extractionCancellation?.Cancel();

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
