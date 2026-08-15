using System.Windows;
using System.IO;
using System.Net.Http;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public partial class MainViewModel : ObservableObject
{
    private readonly DeviceSessionService? deviceSessionService;
    private readonly IDeviceMonitor? deviceMonitor;
    private readonly IOperationCoordinator? coordinator;
    private readonly Func<Task>? onLogout;
    private readonly System.Windows.Threading.DispatcherTimer? clockTimer;
    private int refreshInProgress;

    [ObservableProperty]
    private AppPage selectedPage = AppPage.Overview;

    [ObservableProperty]
    private string accountName = "";

    [ObservableProperty]
    private string currentTimeText = "";

    public DeviceSessionViewModel DeviceSession { get; }

    public OverviewViewModel Overview { get; }

    public QuickFlashViewModel QuickFlash { get; }

    public MirrorViewModel Mirror { get; }

    public FileManagerViewModel FileManager { get; }

    public LineFlashViewModel LineFlash { get; }

    public PartitionWorkspaceViewModel PartitionWorkspace { get; }

    public RootViewModel Root { get; }

    public FirmwareExtractViewModel FirmwareExtract { get; }

    public SafeFlashViewModel SafeFlash { get; }

    public OnlineViewModel Online { get; }

    public OperationLogViewModel Logs { get; }

    public SoftwareViewModel Software { get; }

    public IOperationCoordinator? Coordinator => coordinator;

    public IRelayCommand<AppPage> SelectPageCommand { get; }

    public IAsyncRelayCommand RefreshDeviceCommand { get; }

    public IAsyncRelayCommand LogoutCommand { get; }

    public MainViewModel(
        DeviceSessionViewModel deviceSession,
        OverviewViewModel? overview = null,
        OperationLogViewModel? logs = null,
        DeviceSessionService? deviceSessionService = null,
        QuickFlashViewModel? quickFlash = null,
        MirrorViewModel? mirror = null,
        FileManagerViewModel? fileManager = null,
        LineFlashViewModel? lineFlash = null,
        RootViewModel? root = null,
        PartitionWorkspaceViewModel? partitionWorkspace = null,
        FirmwareExtractViewModel? firmwareExtract = null,
        SafeFlashViewModel? safeFlash = null,
        IDeviceMonitor? deviceMonitor = null,
        IOperationCoordinator? coordinator = null,
        OnlineViewModel? online = null,
        SoftwareViewModel? software = null,
        Func<Task>? onLogout = null)
    {
        var fallbackLogs = new OperationLogService();
        var fallbackCliRunner = new FastbootCliRunner(Path.Combine(Path.GetTempPath(), "unavailable-fastboot.exe"));
        var fallbackOtaClient = new OtaApiClient();
        var unavailableBackend = new FastbootRsBackend(new UnavailableNativeApi());
        this.deviceSessionService = deviceSessionService;
        this.deviceMonitor = deviceMonitor;
        this.coordinator = coordinator;
        DeviceSession = deviceSession;
        Overview = overview ?? new OverviewViewModel(deviceSession, unavailableBackend, fallbackLogs);
        Logs = logs ?? new OperationLogViewModel(fallbackLogs);
        QuickFlash = quickFlash ?? new QuickFlashViewModel(deviceSession, new QuickFlashService(unavailableBackend, fallbackCliRunner, fallbackLogs), fallbackLogs);
        Mirror = mirror ?? new MirrorViewModel(deviceSession, new MirrorService(new SystemProcessRunner(), fallbackLogs));
        FileManager = fileManager ?? new FileManagerViewModel(deviceSession, new AdbFileService(unavailableBackend, fallbackLogs), fallbackLogs);
        LineFlash = lineFlash ?? new LineFlashViewModel(deviceSession, new FastbootPartitionService(fallbackCliRunner), fallbackLogs);
        var workspaceCoordinator = coordinator ?? new OperationCoordinator(deviceSession, fallbackLogs);
        var fallbackFastbootTransport = new FastbootPartitionTransport(fallbackCliRunner);
        var fallbackAdbRootTransport = new AdbRootPartitionTransport(new UnavailableAdbRootTransferRunner());
        PartitionWorkspace = partitionWorkspace ?? new PartitionWorkspaceViewModel(
            deviceSession,
            fallbackFastbootTransport,
            fallbackAdbRootTransport,
            new PartitionExecutionService(deviceSession, workspaceCoordinator, fallbackLogs, [fallbackFastbootTransport, fallbackAdbRootTransport]),
            fallbackLogs,
            workspaceCoordinator);
        Root = root ?? new RootViewModel(deviceSession, new QuickFlashService(unavailableBackend, fallbackCliRunner, fallbackLogs), fallbackLogs);
        FirmwareExtract = firmwareExtract ?? new FirmwareExtractViewModel(fallbackLogs);
        SafeFlash = safeFlash ?? new SafeFlashViewModel(
            deviceSession,
            fallbackLogs,
            unavailableBackend,
            new OtaApiClient(),
            new OtaDownloadService(),
            new FirmwarePartitionExtractor(payloadDumper: null),
            coordinator,
            fallbackCliRunner);
        Online = online ?? new OnlineViewModel(fallbackOtaClient, new HeartbeatService(fallbackOtaClient));
        Software = software ?? new SoftwareViewModel();
        this.onLogout = onLogout;
        LogoutCommand = new AsyncRelayCommand(LogoutAsync, CanLogout);
        if (coordinator is not null)
        {
            coordinator.StateChanged += (_, _) => LogoutCommand.NotifyCanExecuteChanged();
        }
        // DeviceSession.IsBusy 兜底:个别页面(如 LineFlash 历史遗留)走 session.BeginOperation
        // 而非协调器,只有 coordinator.StateChanged 会漏判;会话忙即禁用登出。
        deviceSession.PropertyChanged += (_, args) =>
        {
            if (args.PropertyName is nameof(DeviceSessionViewModel.IsBusy))
            {
                LogoutCommand.NotifyCanExecuteChanged();
            }
        };
        // 纯单测环境(无 WPF Application)不启动时钟,避免 DispatcherTimer 泄漏。
        if (Application.Current is not null)
        {
            clockTimer = new System.Windows.Threading.DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
            clockTimer.Tick += (_, _) => CurrentTimeText = DateTime.Now.ToString("MM-dd HH:mm:ss");
            clockTimer.Start();
            CurrentTimeText = DateTime.Now.ToString("MM-dd HH:mm:ss");
        }

        SelectPageCommand = new RelayCommand<AppPage>(page => SelectedPage = page);
        RefreshDeviceCommand = new AsyncRelayCommand(() => RefreshDeviceAsync(logActivity: true));
    }

    private async Task LogoutAsync()
    {
        if (onLogout is not null)
        {
            await onLogout();
        }
    }

    /// <summary>运行中的操作(刷写/传输等)未结束前禁用登出,避免打断设备操作后仍拆线。</summary>
    private bool CanLogout() =>
        (coordinator is null || !coordinator.IsBusy) && !DeviceSession.IsBusy;

    public void StopClock() => clockTimer?.Stop();

    public async Task RefreshDeviceAsync(bool logActivity, bool automatic = false)
    {
        if (deviceMonitor is not null)
        {
            if (!automatic)
            {
                await deviceMonitor.RefreshManualAsync(logActivity);
            }

            return;
        }

        if (DeviceSession.IsBusy || Interlocked.Exchange(ref refreshInProgress, 1) != 0)
        {
            return;
        }

        try
        {
            if (deviceSessionService is not null)
            {
                await deviceSessionService.RefreshAsync(
                    DeviceSession,
                    CancellationToken.None,
                    logActivity,
                    automatic ? DeviceRefreshMode.Automatic : DeviceRefreshMode.Manual);
            }

            await OnDeviceRefreshedAsync(
                automatic ? DeviceRefreshMode.Automatic : DeviceRefreshMode.Manual,
                CancellationToken.None);
        }
        finally
        {
            Volatile.Write(ref refreshInProgress, 0);
        }
    }

    public async Task OnDeviceRefreshedAsync(DeviceRefreshMode refreshMode, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (DeviceSession.IsBusy)
        {
            return;
        }

        // 可视刷写的分区表只在用户点击「读取分区表」时读取;设备心跳、操作完成后的补偿刷新
        // 都不应触发重读。镜像协调与分区表无关,设备变化时仍应执行。
        await Mirror.ReconcileAsync();
    }

    private sealed class UnavailableNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command) => throw new InvalidOperationException("设备服务未初始化。");
        public string GetVar(string? serial, string variable) => throw new InvalidOperationException("设备服务未初始化。");
        public void Reboot(string? serial, string target) => throw new InvalidOperationException("设备服务未初始化。");
        public void Push(string? serial, string localPath, string remotePath) => throw new InvalidOperationException("设备服务未初始化。");
        public long Pull(string? serial, string remotePath, string localPath) => throw new InvalidOperationException("设备服务未初始化。");
        public string Install(string? serial, string apkPath, bool replace) => throw new InvalidOperationException("设备服务未初始化。");
        public void Flash(string? serial, string partition, string imagePath) => throw new InvalidOperationException("设备服务未初始化。");
    }

    private sealed class UnavailableAdbRootTransferRunner : IAdbRootTransferRunner
    {
        private static InvalidOperationException CreateException() => new("设备服务未初始化。");

        public Task<string> RunRootAsync(string serial, string command, CancellationToken cancellationToken) => Task.FromException<string>(CreateException());

        public Task CopyFromDeviceAsync(string serial, string devicePath, string localPath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.FromException(CreateException());

        public Task CopyToDeviceAsync(string serial, string localImagePath, string devicePath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.FromException(CreateException());

        public Task EraseAsync(string serial, string devicePath, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken) => Task.FromException(CreateException());
    }
}
