using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Windows.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

/// <summary>
/// 安全刷写:查询设备(adb)→ 下载 OTA(或选择本地 .zip / payload.bin)→ 解压解包
/// → 跳过 preloader*/lk → 重启到 fastbootd → 逐个刷入其余分区 → 重启设备。
/// 页面只有「下载+刷入」与「选择固件」两个主操作,刷写前有内联确认。
/// </summary>
public partial class SafeFlashViewModel : ObservableObject
{
    private readonly DeviceSessionViewModel session;
    private readonly OperationLogService logs;
    private readonly FastbootRsBackend backend;
    private readonly IFastbootCliRunner cliRunner;
    private readonly OtaApiClient otaClient;
    private readonly OtaDownloadService downloader;
    private readonly FirmwarePartitionExtractor extractor;
    private readonly IOperationCoordinator? coordinator;
    private readonly Stopwatch operationStopwatch = new();
    private DispatcherTimer? elapsedTimer;
    private CancellationTokenSource? operationCancellation;

    private static readonly string[] VersionCandidates =
    [
        "ro.build.display.id",
        "ro.build.version.incremental",
        "ro.vivo.os.build.display.id"
    ];

    /// <summary>
    /// vivo 权威版本号在 ro.build.version.bbk,形如 "DPD2221B_A_15.2.12.0.W10.V000L1":
    /// 第一段是设备代号(DPD2221B),最后一段是完整版本号(15.2.12.0.W10.V000L1)。
    /// </summary>
    internal static (string Codename, string Version) ParseBbkVersion(string value)
    {
        var parts = value.Split('_', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0)
        {
            return (string.Empty, string.Empty);
        }

        if (parts.Length == 1)
        {
            return (parts[0], parts[0]);
        }

        return (parts[0], parts[^1]);
    }

    private string stagingRoot = string.Empty;
    private string sourcePath = string.Empty;
    private IReadOnlyList<PayloadPartitionEntry> pendingPartitions = [];
    private long lastExtractSpeedBytes;
    private long lastExtractSpeedTicks;

    [ObservableProperty]
    private string deviceSummary = "未连接 ADB 设备";

    [ObservableProperty]
    private bool isBusy;

    [ObservableProperty]
    private string statusText = "等待操作";

    [ObservableProperty]
    private string speedText = "--";

    [ObservableProperty]
    private string elapsedText = "00:00";

    [ObservableProperty]
    private double overallProgress;

    [ObservableProperty]
    private double currentPartitionProgress;

    [ObservableProperty]
    private string currentPartition = "--";

    [ObservableProperty]
    private int flashCount;

    [ObservableProperty]
    private bool isConfirmVisible;

    [ObservableProperty]
    private string confirmSummary = string.Empty;

    public SafeFlashViewModel(
        DeviceSessionViewModel session,
        OperationLogService logs,
        FastbootRsBackend backend,
        OtaApiClient otaClient,
        OtaDownloadService downloader,
        FirmwarePartitionExtractor extractor,
        IOperationCoordinator? coordinator = null,
        IFastbootCliRunner cliRunner = null!)
    {
        this.session = session;
        this.logs = logs;
        this.backend = backend;
        this.cliRunner = cliRunner;
        this.otaClient = otaClient;
        this.downloader = downloader;
        this.extractor = extractor;
        this.coordinator = coordinator;

        // 下载+刷入 的 CanExecute 依赖设备连接状态,必须订阅变化才能刷新按钮可用性。
        session.PropertyChanged += OnSessionPropertyChanged;
        if (coordinator is not null)
        {
            // 本页操作与全局协调器共享同一把锁:别的页面有任务在跑时,本页的开始按钮
            // 必须禁用而不是静默排队(否则点了像卡住);取消按钮则要按全局忙碌状态启用。
            coordinator.StateChanged += OnCoordinatorStateChanged;
        }

        DownloadAndFlashCommand = new AsyncRelayCommand(DownloadAndFlashAsync, CanDownloadAndFlash);
        SelectAndFlashCommand = new AsyncRelayCommand(SelectAndFlashAsync, CanSelectAndFlash);
        ConfirmFlashCommand = new AsyncRelayCommand(ConfirmFlashAsync, CanConfirmFlash);
        CancelFlashCommand = new RelayCommand(CancelFlash, CanConfirmFlash);
        StopCommand = new RelayCommand(Stop, CanStop);
    }

    public IAsyncRelayCommand DownloadAndFlashCommand { get; }

    public IAsyncRelayCommand SelectAndFlashCommand { get; }

    public IAsyncRelayCommand ConfirmFlashCommand { get; }

    public IRelayCommand CancelFlashCommand { get; }

    public IRelayCommand StopCommand { get; }

    public string OverallProgressPercent => IsBusy ? $"{OverallProgress:P0}" : "--";

    /// <summary>空闲或当前分区无逐字节进度(fastboot flash/下载中)时显示 "--"。</summary>
    public string CurrentPartitionProgressPercent =>
        IsBusy && CurrentPartitionProgress > 0 ? $"{CurrentPartitionProgress:P0}" : "--";

    /// <summary>当前分区无可量化的字节进度(fastboot flash 阶段)时用不确定动画。</summary>
    public bool IsCurrentPartitionIndeterminate => IsBusy && CurrentPartitionProgress <= 0;

    partial void OnOverallProgressChanged(double value) => OnPropertyChanged(nameof(OverallProgressPercent));

    partial void OnCurrentPartitionProgressChanged(double value)
    {
        OnPropertyChanged(nameof(CurrentPartitionProgressPercent));
        OnPropertyChanged(nameof(IsCurrentPartitionIndeterminate));
    }

    partial void OnIsBusyChanged(bool value)
    {
        OnPropertyChanged(nameof(IsCurrentPartitionIndeterminate));
        OnPropertyChanged(nameof(OverallProgressPercent));
        OnPropertyChanged(nameof(CurrentPartitionProgressPercent));
        DownloadAndFlashCommand.NotifyCanExecuteChanged();
        SelectAndFlashCommand.NotifyCanExecuteChanged();
        ConfirmFlashCommand.NotifyCanExecuteChanged();
        CancelFlashCommand.NotifyCanExecuteChanged();
        StopCommand.NotifyCanExecuteChanged();
    }

    partial void OnIsConfirmVisibleChanged(bool value) => OnConfirmAvailabilityChanged();

    partial void OnFlashCountChanged(int value) => OnConfirmAvailabilityChanged();

    private void OnConfirmAvailabilityChanged()
    {
        DownloadAndFlashCommand.NotifyCanExecuteChanged();
        SelectAndFlashCommand.NotifyCanExecuteChanged();
        ConfirmFlashCommand.NotifyCanExecuteChanged();
        CancelFlashCommand.NotifyCanExecuteChanged();
    }

    private void OnSessionPropertyChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName is nameof(DeviceSessionViewModel.ConnectionState) or nameof(DeviceSessionViewModel.IsAdbConnected))
        {
            DownloadAndFlashCommand.NotifyCanExecuteChanged();
        }
    }

    private bool CanDownloadAndFlash() => !IsBusy && !IsConfirmVisible && session.IsAdbConnected;

    private bool CanSelectAndFlash() => !IsBusy && !IsConfirmVisible;

    private bool CanConfirmFlash() => IsConfirmVisible && FlashCount > 0 && !IsBusy;

    /// <summary>
    /// 停止按钮:本页有任务(含排队中)或任意页面有任务在跑时都可用——用户在任何菜单
    /// 都能停掉正在跑的操作,而不是因为「本页不忙」而点了没反应。
    /// </summary>
    private bool CanStop() => IsBusy || coordinator?.IsBusy == true;

    /// <summary>全局协调器忙碌状态变化(含其它页面的任务)时刷新本页命令可用性。</summary>
    private void OnCoordinatorStateChanged(object? sender, EventArgs eventArgs)
    {
        DownloadAndFlashCommand.NotifyCanExecuteChanged();
        SelectAndFlashCommand.NotifyCanExecuteChanged();
        ConfirmFlashCommand.NotifyCanExecuteChanged();
        CancelFlashCommand.NotifyCanExecuteChanged();
        StopCommand.NotifyCanExecuteChanged();
    }

    /// <summary>① 下载+刷入:读设备版本 → 查 OTA → 下载 → 准备刷写。</summary>
    private async Task DownloadAndFlashAsync()
    {
        if (session.ConnectionState != DeviceConnectionState.AdbConnected || string.IsNullOrWhiteSpace(session.Serial))
        {
            StatusText = "请先连接 ADB 设备再开始";
            logs.Write(OperationLogLevel.Warning, StatusText);
            return;
        }

        var serial = session.Serial;
        var pd = string.IsNullOrWhiteSpace(session.Details.Codename) ? string.Empty : session.Details.Codename;
        DeviceSummary = pd;
        var (version, codename) = await ReadDeviceVersionAsync(serial);
        if (string.IsNullOrWhiteSpace(pd) || string.IsNullOrWhiteSpace(version))
        {
            StatusText = "无法从设备读取 PD/版本号";
            logs.Write(OperationLogLevel.Warning, StatusText);
            return;
        }

        DeviceSummary = codename is { Length: > 0 }
            ? $"{pd} ({codename}) · {version}"
            : $"{pd} · {version}";
        stagingRoot = CreateStagingRoot();
        Directory.CreateDirectory(stagingRoot);

        var downloadSuccess = await RunOperationAsync(OperationKind.Transferring, "下载 OTA 包", async (context, ct) =>
        {
            context.ReportStage("正在请求 OTA 服务端");
            otaClient.BaseUrl = OtaApiClient.DefaultBaseUrl;
            var rom = await otaClient.ResolveAsync(pd, version, ct);

            var fileName = $"{SanitizeFileNamePart(pd)}_{SanitizeFileNamePart(version)}_ota.zip";
            var destination = Path.Combine(stagingRoot, fileName);
            context.ReportStage("正在多线程下载 OTA 包");
            StartElapsedTicker();
            try
            {
                var progress = new Progress<DownloadProgress>(OnDownloadProgress);
                await downloader.DownloadAsync(new Uri(rom.Url), destination, progress, ct);
                sourcePath = destination;
                context.ReportProgress(1);
            }
            finally
            {
                StopElapsedTicker();
            }
        });

        // 下载失败(查询不到 / 下载出错)就停下并清理 staging,不再继续解包刷写。
        if (!downloadSuccess || string.IsNullOrWhiteSpace(sourcePath))
        {
            CleanupStaging();
            return;
        }

        logs.Write(OperationLogLevel.Info, $"已选择固件 {sourcePath}");
        await PrepareFlashAsync();
    }

    /// <summary>② 选择固件:本地 .zip / payload.bin → 准备刷写。</summary>
    private async Task SelectAndFlashAsync()
    {
        var dialog = new OpenFileDialog
        {
            Filter = "Vivo 固件 (*.zip;*.bin)|*.zip;*.bin|全部文件 (*.*)|*.*",
            CheckFileExists = true,
            Title = "选择 OTA zip 或 payload.bin"
        };
        if (dialog.ShowDialog() != true)
        {
            return;
        }

        stagingRoot = CreateStagingRoot();
        Directory.CreateDirectory(stagingRoot);
        sourcePath = dialog.FileName;
        logs.Write(OperationLogLevel.Info, $"已选择固件 {sourcePath}");
        await PrepareFlashAsync();
    }

    /// <summary>读取源中的待刷分区,弹内联确认(列出跳过 preloader*/lk 后的分区数)。</summary>
    private async Task PrepareFlashAsync()
    {
        var success = await RunOperationAsync(OperationKind.Hashing, "分析固件", async (context, ct) =>
        {
            context.ReportStage("正在读取分区列表");
            var partitions = await extractor.ListPartitionsAsync(sourcePath, ct);
            pendingPartitions = partitions;
            FlashCount = partitions.Count;
            context.ReportProgress(1);
        });
        if (!success)
        {
            CleanupStaging();
            return;
        }

        if (FlashCount == 0)
        {
            StatusText = "固件中未找到可刷写分区。";
            logs.Write(OperationLogLevel.Warning, StatusText);
            CleanupStaging();
            return;
        }

        logs.Write(OperationLogLevel.Info, $"固件含 {FlashCount} 个分区(已跳过 preloader/lk)");
        foreach (var partition in pendingPartitions)
        {
            logs.Write(OperationLogLevel.Info, $"{partition.Name}.img > {partition.Name} | {FormatBytes(partition.SizeBytes)}");
        }

        ConfirmSummary = $"将刷入 {FlashCount} 个分区(已跳过 preloader/lk),随后重启到 fastbootd 逐个刷写。";
        if (extractor.HasBlockBasedContent(sourcePath))
        {
            var blockWarning = "⚠ 固件含块式分区内容(.new.dat / transfer.list,如 system/vendor/product),暂不支持刷写,本次只会刷可直接镜像的分区,其余保持原样。";
            logs.Write(OperationLogLevel.Warning, blockWarning);
            ConfirmSummary += Environment.NewLine + blockWarning;
        }

        IsConfirmVisible = true;
    }

    private void CancelFlash()
    {
        IsConfirmVisible = false;
        pendingPartitions = [];
        FlashCount = 0;
        StatusText = "已取消刷写";
        // 取消后必须释放已下载的 staging(数 GB OTA),否则每次取消/重试都累积残留。
        CleanupStaging();
    }

    /// <summary>③ 确认后:解压解包 → 重启 fastbootd → 逐个刷入 → 重启设备。</summary>
    private async Task ConfirmFlashAsync()
    {
        if (!IsConfirmVisible || pendingPartitions.Count == 0)
        {
            return;
        }

        var partitionsToFlash = pendingPartitions;
        IsConfirmVisible = false;
        await RunOperationAsync(OperationKind.Flashing, "VIVO 线刷", async (context, ct) =>
        {
            var extractDirectory = Path.Combine(stagingRoot, "extract", Guid.NewGuid().ToString("N"));

            // 1. 解压解包所有分区镜像。先确认 staging 盘放得下全部镜像(OTA 包已在 staging)。
            var taskStopwatch = Stopwatch.StartNew();
            OtaDownloadService.EnsureDiskSpace(
                extractDirectory,
                partitionsToFlash.Sum(partition => Math.Max(partition.SizeBytes, 0)));
            context.ReportStage("正在解压解包固件");
            var images = new List<SafeFlashImageInfo>(partitionsToFlash.Count);
            for (var index = 0; index < partitionsToFlash.Count; index++)
            {
                ct.ThrowIfCancellationRequested();
                var partition = partitionsToFlash[index];
                var capturedIndex = index;
                var size = partition.SizeBytes;
                CurrentPartition = partition.Name;
                CurrentPartitionProgress = 0;
                lastExtractSpeedBytes = 0;
                lastExtractSpeedTicks = 0;
                context.ReportStage($"正在解包 {partition.Name}({index + 1}/{partitionsToFlash.Count})", OperationKind.Hashing);
                var writeProgress = new Progress<long>(bytes =>
                {
                    var fraction = size > 0 ? Math.Clamp(bytes / (double)size, 0, 1) : 0;
                    CurrentPartitionProgress = fraction;
                    OverallProgress = 0.5 * (capturedIndex + fraction) / partitionsToFlash.Count;
                    UpdateExtractSpeed(bytes);
                });
                var image = await extractor.ExtractPartitionAsync(sourcePath, partition.Name, extractDirectory, ct, writeProgress);
                images.Add(image);
            }

            // 2. 重启到 fastbootd(vivo 刷写一律在 fastbootd)。
            if (session.ConnectionState == DeviceConnectionState.AdbConnected && !string.IsNullOrWhiteSpace(session.Serial))
            {
                logs.Write(OperationLogLevel.Info, "等待Fastboot设备...");
                context.ReportStage("正在重启到 fastbootd");
                await backend.RebootAsync(session.Serial, "fastboot", ct); // adb reboot fastboot
                if (!await WaitForFastbootAsync(ct))
                {
                    throw new TimeoutException("设备重启后未检测到 fastboot 设备。");
                }

                logs.Write(OperationLogLevel.Info, $"已连接 {session.Serial} | 用时 {taskStopwatch.Elapsed.TotalSeconds:0} 秒");
                // 设备刚进入 fastbootd 时 USB 可能仍在枚举;等一小段再发 getvar,
                // 避免瞬态探测失败被误判为“设备无此分区”而静默跳过必需分区。
                logs.Write(OperationLogLevel.Info, "等待设备连接稳定...");
                await Task.Delay(TimeSpan.FromSeconds(1.5), ct);
            }
            else if (session.ConnectionState != DeviceConnectionState.FastbootConnected || string.IsNullOrWhiteSpace(session.Serial))
            {
                throw new InvalidOperationException("请先连接 ADB 或 fastboot 设备再刷写。");
            }

            // 3. 逐个刷入(其余全刷,preloader*/lk 已过滤)。用当前 session.Serial,
            //    并依赖 fastboot 调用自身在设备断开时报错——不额外校验会话状态,
            //    避免长时间刷大分区时监控瞬时抖动导致误中止。
            var skipped = 0;
            for (var index = 0; index < images.Count; index++)
            {
                ct.ThrowIfCancellationRequested();
                var serial = session.Serial;
                if (string.IsNullOrWhiteSpace(serial) || session.ConnectionState != DeviceConnectionState.FastbootConnected)
                {
                    throw new InvalidOperationException("刷写过程中设备连接已断开。");
                }

                var image = images[index];
                // 设备上不存在的分区(如 OTA 里各区域变体专属的分区)先 getvar 探一下,
                // 不存在就跳过,避免未知分区导致整条流程中止、设备被刷到一半。
                if (!await PartitionExistsAsync(serial, image.PartitionName, ct))
                {
                    logs.Write(OperationLogLevel.Warning, $"设备无 {image.PartitionName} 分区,已跳过。");
                    skipped++;
                    continue;
                }

                CurrentPartition = image.PartitionName;
                // fastboot CLI 带连续传输进度(进程写字节/镜像大小),实时更新右侧栏当前分区进度 + 速度。
                CurrentPartitionProgress = 0;
                lastExtractSpeedBytes = 0;
                lastExtractSpeedTicks = 0;
                SpeedText = string.Empty;
                context.ReportStage($"正在刷写 {image.PartitionName}({index + 1}/{images.Count})", OperationKind.Flashing);
                var flashWatch = Stopwatch.StartNew();
                logs.Write(OperationLogLevel.Info, $"Sending '{image.PartitionName}' ({Math.Max(image.SizeBytes / 1024, 1)} KB)");
                var flashProgress = new Progress<double>(fraction =>
                {
                    CurrentPartitionProgress = fraction;
                    OverallProgress = 0.5 + ((index + fraction) / images.Count) * 0.5;
                    UpdateExtractSpeed((long)(fraction * image.SizeBytes));
                });
                // fastboot CLI 会打印可读错误(无设备/镜像缺失/设备 FAIL 消息)。
                await cliRunner.FlashAsync(serial, image.PartitionName, image.ImagePath, flashProgress, ct);
                flashWatch.Stop();
                var seconds = flashWatch.Elapsed.TotalSeconds;
                logs.Write(OperationLogLevel.Info, $"OKAY [  {seconds:0.3f}s]");
                logs.Write(OperationLogLevel.Info, $"Writing '{image.PartitionName}'                               OKAY [  {seconds:0.3f}s]");
                logs.Write(OperationLogLevel.Info, $"Finished. Total time: {seconds:0.3f}s");
                logs.Write(OperationLogLevel.Info, $"Flashing {Path.GetFileName(image.ImagePath)}...OK");
                OverallProgress = 0.5 + ((index + 1) / (double)images.Count) * 0.5;
            }

            // 4. 重启回系统。
            logs.Write(OperationLogLevel.Info, "[Rebooting]发送重启命令...");
            context.ReportStage("正在重启设备");
            await cliRunner.RebootAsync(session.Serial, ct); // fastboot reboot

            OverallProgress = 1;
            taskStopwatch.Stop();
            logs.Write(OperationLogLevel.Success, $"任务结束,耗时{taskStopwatch.Elapsed.TotalSeconds:0.0}秒.");
            StatusText = skipped > 0
                ? $"已刷入 {images.Count - skipped} 个分区,跳过 {skipped} 个设备不存在的分区"
                : $"已刷入 {images.Count} 个分区";
            logs.Write(OperationLogLevel.Success, StatusText);
        }, cleanup: CleanupStaging);
    }

    /// <summary>读取设备版本号。优先 ro.build.version.bbk(权威,解析出代号+版本),回退候选属性。</summary>
    private async Task<(string Version, string Codename)> ReadDeviceVersionAsync(string serial)
    {
        try
        {
            var bbk = (await backend.ShellAsync(serial, "getprop ro.build.version.bbk", CancellationToken.None)).Trim();
            if (!string.IsNullOrWhiteSpace(bbk))
            {
                var (codename, version) = ParseBbkVersion(bbk);
                if (!string.IsNullOrWhiteSpace(version) && !IsGenericVersion(version))
                {
                    return (version, codename);
                }
            }
        }
        catch
        {
            // 回退到候选属性。
        }

        foreach (var prop in VersionCandidates)
        {
            try
            {
                var value = (await backend.ShellAsync(serial, $"getprop {prop}", CancellationToken.None)).Trim();
                if (!string.IsNullOrWhiteSpace(value) && !IsGenericVersion(value))
                {
                    return (value, string.Empty);
                }
            }
            catch
            {
                // 尝试下一个候选属性。
            }
        }

        return (string.Empty, string.Empty);
    }

    private static bool IsGenericVersion(string value) =>
        value.Contains("release-keys", StringComparison.OrdinalIgnoreCase) ||
        value.Equals("unknown", StringComparison.OrdinalIgnoreCase) ||
        value.Equals("not found", StringComparison.OrdinalIgnoreCase);

    private async Task<bool> WaitForFastbootAsync(CancellationToken cancellationToken, int timeoutSeconds = 90)
    {
        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(timeoutSeconds);
        while (DateTime.UtcNow < deadline)
        {
            cancellationToken.ThrowIfCancellationRequested();

            // 直接轮询 fastboot 后端,而不是 session:操作期间 coordinator.IsBusy
            // 会冻结 DeviceMonitor/DeviceSession 的自动刷新,session.ConnectionState
            // 不会更新(adb reboot fastboot 后永远停在 AdbConnected),必须实时探测。
            // 设备重启过渡期后端可能瞬时报错,吞掉继续等到超时,而非中断整条刷写。
            try
            {
                var device = await backend.DiscoverAsync(cancellationToken);
                if (device.ConnectionState == DeviceConnectionState.FastbootConnected)
                {
                    session.ApplyDevice(device);
                    return true;
                }
            }
            catch (OperationCanceledException)
            {
                throw;
            }
            catch
            {
                // 过渡期探测失败视为设备尚未现身,继续等待。
            }

            await Task.Delay(500, cancellationToken);
        }

        return false;
    }

    /// <summary>用 <c>getvar partition-type:&lt;name&gt;</c> 探测分区是否存在于设备。</summary>
    private async Task<bool> PartitionExistsAsync(string serial, string partition, CancellationToken cancellationToken)
        => await cliRunner.PartitionExistsAsync(serial, partition, cancellationToken);

    /// <summary>
    /// 选 staging 盘:系统盘(通常是 SSD,bezzad 多分片随机写才不卡)有 ≥15GB 空闲就优先,
    /// 否则选剩余空间最大的固定盘。
    /// </summary>
    private static string CreateStagingRoot()
    {
        var fixedDrives = DriveInfo.GetDrives()
            .Where(info => info.IsReady && info.DriveType == DriveType.Fixed)
            .ToArray();
        var systemRoot = Path.GetPathRoot(Environment.SystemDirectory);
        var systemDrive = fixedDrives.FirstOrDefault(info =>
            string.Equals(info.Name, systemRoot, StringComparison.OrdinalIgnoreCase));
        var drive = systemDrive is { AvailableFreeSpace: >= 15L * 1024 * 1024 * 1024 }
            ? systemDrive
            : fixedDrives.OrderByDescending(info => info.AvailableFreeSpace).FirstOrDefault();
        var root = drive?.Name ?? Path.GetPathRoot(Path.GetTempPath())!;
        return Path.Combine(root, "VivoKsu", "safe-flash", Guid.NewGuid().ToString("N"));
    }

    private void CleanupStaging()
    {
        if (string.IsNullOrWhiteSpace(stagingRoot))
        {
            return;
        }

        try
        {
            if (Directory.Exists(stagingRoot))
            {
                Directory.Delete(stagingRoot, recursive: true);
            }
        }
        catch
        {
            // Best effort; staging is swept by the OS eventually.
        }

        stagingRoot = string.Empty;
        sourcePath = string.Empty;
    }

    private void OnDownloadProgress(DownloadProgress progress)
    {
        if (progress.TotalBytes is { } total && total > 0)
        {
            OverallProgress = Math.Clamp(progress.DownloadedBytes / (double)total, 0, 1);
        }

        if (progress.BytesPerSecond > 0)
        {
            SpeedText = FormatBytes((long)progress.BytesPerSecond) + "/s";
        }
    }

    /// <summary>按解包写入字节的时间差计算 MB/s 速度(右侧栏与页底速度显示)。</summary>
    private void UpdateExtractSpeed(long bytes)
    {
        var now = Environment.TickCount64;
        if (lastExtractSpeedBytes > 0 && now > lastExtractSpeedTicks)
        {
            var deltaBytes = bytes - lastExtractSpeedBytes;
            var deltaSeconds = (now - lastExtractSpeedTicks) / 1000.0;
            if (deltaBytes > 0 && deltaSeconds > 0)
            {
                SpeedText = FormatBytes((long)(deltaBytes / deltaSeconds)) + "/s";
            }
        }

        lastExtractSpeedBytes = bytes;
        lastExtractSpeedTicks = now;
    }

    private void StartElapsedTicker()
    {
        operationStopwatch.Restart();
        elapsedTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(500) };
        elapsedTimer.Tick += (_, _) => ElapsedText = operationStopwatch.Elapsed.ToString(@"hh\:mm\:ss");
        elapsedTimer.Start();
    }

    private void StopElapsedTicker()
    {
        elapsedTimer?.Stop();
        elapsedTimer = null;
        operationStopwatch.Stop();
        ElapsedText = operationStopwatch.Elapsed.ToString(@"hh\:mm\:ss");
    }

    private void Stop()
    {
        // 优先取消本页自己的操作(含排队等锁中的);本页无任务时,取消全局正在运行的
        // 操作,让用户在其他菜单也能停掉别的页面发起的任务。
        if (operationCancellation is not null)
        {
            operationCancellation.Cancel();
            return;
        }

        coordinator?.CancelCurrent();
    }

    /// <summary>运行一个阶段化操作;失败/取消时把错误写到状态与日志并返回 false。</summary>
    private async Task<bool> RunOperationAsync(
        OperationKind kind,
        string title,
        Func<OperationContext, CancellationToken, Task> operation,
        Action? cleanup = null)
    {
        var cancellation = new CancellationTokenSource();
        operationCancellation = cancellation;
        IsBusy = true;
        try
        {
            if (coordinator is not null)
            {
                try
                {
                    await coordinator.RunAsync(kind, title, operation, cancellation.Token);
                    return true;
                }
                catch (OperationCanceledException)
                {
                    StatusText = "操作已取消";
                    logs.Write(OperationLogLevel.Warning, StatusText);
                }
                catch (Exception exception)
                {
                    StatusText = exception.Message;
                    logs.Write(OperationLogLevel.Error, exception.Message);
                }

                return false;
            }

            // 无协调器时回退:自行管理取消与日志。
            try
            {
                await operation(new OperationContext(Guid.NewGuid().ToString("N"), (stage, _, _) =>
                {
                    if (stage is not null)
                    {
                        StatusText = stage;
                        logs.Write(OperationLogLevel.Info, stage);
                    }
                }), cancellation.Token);
                return true;
            }
            catch (OperationCanceledException)
            {
                StatusText = "操作已取消";
            }
            catch (Exception exception)
            {
                StatusText = exception.Message;
                logs.Write(OperationLogLevel.Error, exception.Message);
            }

            return false;
        }
        finally
        {
            cancellation.Dispose();
            operationCancellation = null;
            cleanup?.Invoke();
            CurrentPartition = "--";
            IsBusy = false;
        }
    }

    private static string SanitizeFileNamePart(string value) =>
        string.Concat(value.Where(character => !Path.GetInvalidFileNameChars().Contains(character)));

    /// <summary>测试用:直接注入待刷源与分区,跳过下载/文件对话框,便于验证刷写编排。</summary>
    internal void SetPendingSourceForTesting(
        string source,
        string stagingRoot,
        IReadOnlyList<PayloadPartitionEntry> partitions)
    {
        sourcePath = source;
        this.stagingRoot = stagingRoot;
        pendingPartitions = partitions;
        FlashCount = partitions.Count;
        IsConfirmVisible = true;
    }

    private static string FormatBytes(long bytes) => bytes switch
    {
        < 1024 => $"{bytes} B",
        < 1024 * 1024 => $"{bytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{bytes / 1024d / 1024:F1} MB",
        _ => $"{bytes / 1024d / 1024 / 1024:F2} GB"
    };
}
