using CommunityToolkit.Mvvm.ComponentModel;
using VivoKsu.App.Models;

namespace VivoKsu.App.ViewModels;

/// <summary>组件安装窗中单项资源的状态。</summary>
public enum ResourceDownloadStatus
{
    Pending,
    Downloading,
    Ready,
    Failed,
    Skipped
}

/// <summary>
/// 组件安装窗的单项:展示名称/大小/状态/进度,并持有该资源的实际安装动作。
/// 状态由 <see cref="ResourceDownloadViewModel"/> 驱动;进度经 <see cref="ApplyProgress"/>
/// 从下载器上报的 <see cref="DownloadProgress"/> 映射(字节 → 百分比 + 速度文本)。
/// </summary>
public sealed partial class ResourceDownloadItemViewModel : ObservableObject
{
    private readonly Func<CancellationToken, IProgress<DownloadProgress>, Task> installer;

    public ResourceDownloadItemViewModel(
        string key,
        string displayName,
        string sizeLabel,
        bool isInstalled,
        Func<CancellationToken, IProgress<DownloadProgress>, Task> installer)
    {
        Key = key;
        DisplayName = displayName;
        SizeLabel = sizeLabel;
        IsInstalled = isInstalled;
        this.installer = installer;
        Status = isInstalled ? ResourceDownloadStatus.Ready : ResourceDownloadStatus.Pending;
        StatusText = isInstalled ? "已就绪" : "待下载";
        ProgressText = "0 MB";
    }

    public string Key { get; }

    public string DisplayName { get; }

    public string SizeLabel { get; }

    /// <summary>已就绪(随包或缓存存在):勾选框禁用,不可再选。</summary>
    public bool IsInstalled { get; }

    [ObservableProperty]
    private bool isSelected;

    [ObservableProperty]
    private ResourceDownloadStatus status;

    [ObservableProperty]
    private string statusText = "";

    /// <summary>进度 0-1;总字节未知时为 0 并用 <see cref="IsIndeterminate"/> 显示不确定条。</summary>
    [ObservableProperty]
    private double progress;

    [ObservableProperty]
    private bool isIndeterminate = true;

    [ObservableProperty]
    private string progressText = "";

    /// <summary>执行该资源的实际安装(下载 + 校验 + 落缓存)。进度经 progressSink 上报。</summary>
    public async Task InstallAsync(CancellationToken cancellationToken, IProgress<DownloadProgress> progressSink)
        => await installer(cancellationToken, progressSink);

    /// <summary>把下载器上报的进度映射到 UI(百分比 + 已下载/速度文本)。UI 线程经 Progress 封送调用。</summary>
    public void ApplyProgress(DownloadProgress p)
    {
        Progress = p.TotalBytes is { } total && total > 0
            ? Math.Clamp((double)p.DownloadedBytes / total, 0, 1)
            : 0;
        IsIndeterminate = p.TotalBytes is not ( > 0);
        ProgressText = FormatBytes(p.DownloadedBytes)
            + (p.BytesPerSecond > 0 ? $" · {FormatBytes((long)p.BytesPerSecond)}/s" : string.Empty);
    }

    private static string FormatBytes(long bytes) =>
        bytes >= 1_048_576 ? $"{bytes / 1_048_576.0:0.0} MB" : $"{bytes / 1024.0:0.0} KB";
}
