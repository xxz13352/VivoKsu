using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public partial class MirrorViewModel : ObservableObject
{
    private readonly DeviceSessionViewModel session;
    private readonly MirrorService mirror;
    private readonly ToolPathPreferences? preferences;
    private readonly SynchronizationContext? synchronizationContext;
    private CancellationTokenSource? reconcileCancellation;

    [ObservableProperty]
    private bool autoMirrorEnabled;

    [ObservableProperty]
    private bool isMirroring;

    public MirrorViewModel(DeviceSessionViewModel session, MirrorService mirror, ToolPathPreferences? preferences = null)
    {
        this.session = session;
        this.mirror = mirror;
        this.preferences = preferences;
        RestoreConfiguredTool();
        synchronizationContext = SynchronizationContext.Current;
        mirror.StateChanged += OnMirrorStateChanged;
        IsMirroring = mirror.IsMirroring;
        StartMirrorCommand = new AsyncRelayCommand(StartAsync);
        StopMirrorCommand = new AsyncRelayCommand(StopAsync);
        BrowseScrcpyToolCommand = new RelayCommand(BrowseScrcpyTool);
    }

    public IAsyncRelayCommand StartMirrorCommand { get; }
    public IAsyncRelayCommand StopMirrorCommand { get; }
    public IRelayCommand BrowseScrcpyToolCommand { get; }

    public string MirrorStatusText => IsMirroring ? "投屏运行中" : "投屏未启动";

    public bool IsScreenCastToolAvailable => mirror.IsToolAvailable;

    public string ScreenCastToolStatus => mirror.ToolStatusMessage;

    public void ConfigureScrcpyTool(string toolPath)
    {
        mirror.ConfigureToolPath(toolPath);
        preferences?.SaveScrcpyPath(toolPath);
        OnPropertyChanged(nameof(IsScreenCastToolAvailable));
        OnPropertyChanged(nameof(ScreenCastToolStatus));
    }

    private void RestoreConfiguredTool()
    {
        if (string.IsNullOrWhiteSpace(preferences?.ScrcpyPath))
        {
            return;
        }

        try
        {
            mirror.ConfigureToolPath(preferences.ScrcpyPath);
        }
        catch (Exception)
        {
            preferences.ClearScrcpyPath();
        }
    }

    partial void OnAutoMirrorEnabledChanged(bool value)
    {
        mirror.AutoMirrorEnabled = value;
        if (value)
        {
            // A manual stop latches deliberateStop in the service; re-enabling the
            // toggle must re-arm auto-mirror so it works again after a re-connect.
            mirror.ClearDeliberateStop();
            reconcileCancellation?.Dispose();
            reconcileCancellation = new CancellationTokenSource();
            _ = ReconcileAsync(reconcileCancellation.Token);
            return;
        }

        // 关闭开关:取消 in-flight 的自动投屏流程(可能正在下载 scrcpy),
        // 并停掉已运行的投屏,否则开关关闭后 scrcpy 仍可能被拉起/继续运行。
        reconcileCancellation?.Cancel();
        reconcileCancellation?.Dispose();
        reconcileCancellation = null;
        _ = mirror.StopAsync();
        IsMirroring = mirror.IsMirroring;
    }

    partial void OnIsMirroringChanged(bool value) => OnPropertyChanged(nameof(MirrorStatusText));

    public async Task ReconcileAsync(CancellationToken cancellationToken = default)
    {
        await mirror.ReconcileAsync(session, cancellationToken);
        IsMirroring = mirror.IsMirroring;
        OnPropertyChanged(nameof(IsScreenCastToolAvailable));
        OnPropertyChanged(nameof(ScreenCastToolStatus));
    }

    private async Task StartAsync()
    {
        await mirror.StartAsync(session, CancellationToken.None);
        IsMirroring = mirror.IsMirroring;
        OnPropertyChanged(nameof(IsScreenCastToolAvailable));
        OnPropertyChanged(nameof(ScreenCastToolStatus));
    }

    private async Task StopAsync()
    {
        await mirror.StopAsync();
        UpdateMirrorState();
    }

    private void BrowseScrcpyTool()
    {
        var dialog = new OpenFileDialog
        {
            Filter = "scrcpy 投屏器 (scrcpy.exe)|scrcpy.exe",
            CheckFileExists = true,
            Multiselect = false,
            Title = "选择有效的 scrcpy.exe"
        };

        if (dialog.ShowDialog() == true)
        {
            try
            {
                ConfigureScrcpyTool(dialog.FileName);
            }
            catch
            {
                OnPropertyChanged(nameof(ScreenCastToolStatus));
            }
        }
    }

    private void OnMirrorStateChanged(object? sender, EventArgs eventArgs)
    {
        if (synchronizationContext is not null && SynchronizationContext.Current != synchronizationContext)
        {
            synchronizationContext.Post(_ => UpdateMirrorState(), null);
            return;
        }

        UpdateMirrorState();
    }

    private void UpdateMirrorState() => IsMirroring = mirror.IsMirroring;
}
