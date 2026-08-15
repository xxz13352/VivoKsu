using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

/// <summary>
/// 「组件安装」窗 VM(发布瘦身后 scrcpy / ROOT 管理器 APK×2 / payload_dumper 按需下载)。
/// 职责:检测缺失资源 → 构建 4 项列表;选中项并行下载(每项独立进度);失败隔离;
/// 可选装 / 全部安装 / 跳过 / 取消。不依赖 Window,便于单元测试。
/// </summary>
public sealed partial class ResourceDownloadViewModel : ObservableObject
{
    private readonly ScrcpyProvisioningService scrcpy;
    private readonly PayloadDumperProvisioner payload;
    private readonly VivoRootResourceService resources;
    private CancellationTokenSource? downloadCts;
    private Task? currentBatch;

    public ResourceDownloadViewModel(
        ScrcpyProvisioningService scrcpy,
        PayloadDumperProvisioner payload,
        VivoRootResourceService resources)
    {
        this.scrcpy = scrcpy;
        this.payload = payload;
        this.resources = resources;
        InstallAllCommand = new AsyncRelayCommand(InstallAllAsync, CanInstall);
        InstallSelectedCommand = new AsyncRelayCommand(InstallSelectedAsync, CanInstall);
        SkipCommand = new RelayCommand(() => OnFinished?.Invoke(true));
        CancelCommand = new AsyncRelayCommand(CancelAsync, () => IsDownloading);
    }

    public ObservableCollection<ResourceDownloadItemViewModel> Items { get; } = [];

    /// <summary>窗口收尾回调:参数 true=跳过或全部完成(可关),false=取消。</summary>
    public Action<bool>? OnFinished { get; set; }

    [ObservableProperty]
    private bool hasMissing;

    [ObservableProperty]
    private bool isDownloading;

    [ObservableProperty]
    private string summaryText = "";

    /// <summary>已勾选(待下载)项数。</summary>
    public int SelectedCount => Items.Count(item => item.IsSelected);

    /// <summary>缺失(未就绪)项数。</summary>
    public int MissingCount => Items.Count(item => !item.IsInstalled);

    /// <summary>正在下载中的项数(用于底部「N / M 下载中」)。</summary>
    public int DownloadingCount => Items.Count(item => item.Status == ResourceDownloadStatus.Downloading);

    /// <summary>失败项数(底部汇总行)。</summary>
    public int FailedCount => Items.Count(item => item.Status == ResourceDownloadStatus.Failed);

    public IAsyncRelayCommand InstallAllCommand { get; }

    public IAsyncRelayCommand InstallSelectedCommand { get; }

    public IRelayCommand SkipCommand { get; }

    public IAsyncRelayCommand CancelCommand { get; }

    private bool CanInstall() => !IsDownloading && Items.Any(item => item.IsSelected);

    /// <summary>检测 4 个资源就绪状态,构建列表;缺失项默认勾选。</summary>
    public void Detect()
    {
        Items.Clear();
        AddItem("scrcpy", "scrcpy 投屏", "≈26 MB", scrcpy.IsInstalled,
            (ct, p) => scrcpy.EnsureInstalledAsync(ct, p));
        AddManager("KSU", "KSU 管理器", "≈9.1 MB");
        AddManager("OfficialKsu", "KernelSU 管理器", "≈9.1 MB");
        AddItem("payload", "payload_dumper", "≈1.9 MB", payload.IsAvailable,
            (ct, p) => payload.EnsureInstalledAsync(ct, p));

        HasMissing = Items.Any(item => !item.IsInstalled);
        RefreshCounters();
    }

    /// <summary>全部缺失项并行下载;失败隔离,单项失败不拖垮其余。</summary>
    private async Task InstallAllAsync() => await InstallCoreAsync(Items.Where(item => !item.IsInstalled).ToArray());

    /// <summary>仅下载勾选的缺失项。</summary>
    private async Task InstallSelectedAsync() => await InstallCoreAsync(Items.Where(item => item.IsSelected && !item.IsInstalled).ToArray());

    /// <summary>测试接缝:直接对指定项跑编排(下载/失败隔离/取消),不依赖真实 provisioners。</summary>
    internal Task InstallCoreForTestingAsync(params ResourceDownloadItemViewModel[] targets) => InstallCoreAsync(targets);

    private async Task InstallCoreAsync(ResourceDownloadItemViewModel[] targets)
    {
        if (targets.Length == 0)
        {
            return;
        }

        IsDownloading = true;
        downloadCts = new CancellationTokenSource();
        var token = downloadCts.Token;
        currentBatch = Task.WhenAll(targets.Select(item => RunItemAsync(item, token)));
        try
        {
            await currentBatch;
        }
        finally
        {
            IsDownloading = false;
            downloadCts.Dispose();
            downloadCts = null;
            RefreshCounters();
        }

        // 全部就绪 → 自动收起;有失败留给用户看结果。
        if (Items.All(item => item.Status == ResourceDownloadStatus.Ready))
        {
            OnFinished?.Invoke(true);
        }
    }

    private async Task RunItemAsync(ResourceDownloadItemViewModel item, CancellationToken token)
    {
        item.Status = ResourceDownloadStatus.Downloading;
        item.StatusText = "下载中";
        try
        {
            var progress = new Progress<DownloadProgress>(item.ApplyProgress);
            await item.InstallAsync(token, progress);
            item.Status = ResourceDownloadStatus.Ready;
            item.StatusText = "已就绪";
            item.Progress = 1;
            item.IsIndeterminate = false;
            item.IsSelected = false;
        }
        catch (OperationCanceledException)
        {
            item.Status = ResourceDownloadStatus.Skipped;
            item.StatusText = "已取消";
        }
        catch
        {
            item.Status = ResourceDownloadStatus.Failed;
            item.StatusText = "失败";
        }
    }

    /// <summary>取消在途下载并等批次收尾(用户点「取消」)。</summary>
    private async Task CancelAsync()
    {
        Cancel();
        if (currentBatch is not null)
        {
            try
            {
                await currentBatch;
            }
            catch
            {
                // WhenAll 内已吞异常;此处兜底。
            }
        }

        OnFinished?.Invoke(false);
    }

    /// <summary>仅取消下载器(窗口直接关闭时由 OnClosed 调用,不触发 OnFinished)。</summary>
    public void Cancel() => downloadCts?.Cancel();

    private void AddManager(string key, string displayName, string sizeLabel)
    {
        var installed = resources.IsManagerApkInstalled(key);
        AddItem($"manager-{key}", displayName, sizeLabel, installed,
            (ct, p) => resources.EnsureManagerApkAsync(resources.ResolveManager(key), ct, p));
    }

    /// <summary>构建一项并挂进列表(IsSelected=缺失项勾选);internal 供测试注入 fake installer。</summary>
    internal void AddItem(
        string key,
        string displayName,
        string sizeLabel,
        bool isInstalled,
        Func<CancellationToken, IProgress<DownloadProgress>, Task> installer)
    {
        var item = new ResourceDownloadItemViewModel(key, displayName, sizeLabel, isInstalled, installer)
        {
            IsSelected = !isInstalled
        };
        item.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(ResourceDownloadItemViewModel.IsSelected) or nameof(ResourceDownloadItemViewModel.Status))
            {
                RefreshCounters();
                InstallAllCommand.NotifyCanExecuteChanged();
                InstallSelectedCommand.NotifyCanExecuteChanged();
            }
        };
        Items.Add(item);
    }

    private void RefreshCounters()
    {
        OnPropertyChanged(nameof(SelectedCount));
        OnPropertyChanged(nameof(MissingCount));
        OnPropertyChanged(nameof(DownloadingCount));
        OnPropertyChanged(nameof(FailedCount));
        SummaryText = FailedCount > 0
            ? $"缺失 {MissingCount} 项 · {FailedCount} 项失败,可稍后在软件页重试"
            : $"缺失 {MissingCount} 项 · 已选 {SelectedCount} 项";
    }
}
