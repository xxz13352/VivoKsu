using System.IO;
using System.Net.Http;
using System.Windows;
using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class AppComposition
{
    private readonly MirrorService mirrorService;
    private readonly OtaApiClient otaClient;
    private readonly UsageLogUploader usageReporter;
    private readonly RemoteAssetDownloader assetDownloader;
    private readonly ScrcpyProvisioningService scrcpyProvisioner;
    private readonly PayloadDumperProvisioner payloadDumperProvisioner;
    private readonly VivoRootResourceService rootResources;
    private bool stopped;

    private AppComposition(
        IFastbootRsNativeApi nativeApi,
        IProcessRunner processRunner,
        ToolPathPreferences? preferences = null,
        Action<string>? notifyBlocked = null,
        bool enableServerGate = true,
        UsageLogUploader? usageReporter = null,
        string? operationsLogPath = null)
    {
        var backend = new FastbootRsBackend(nativeApi);
        // 唯一 fastboot 执行器:全部刷写 / 读变量 / 擦除 / 重启 / 槽位操作走它(fastboot-rs DLL 已移除)。
        var cliRunner = new FastbootCliRunner(
            Path.Combine(AppContext.BaseDirectory, "platform-tools", "fastboot.exe"));
        LogService = new OperationLogService(operationsLogPath);
        Session = new DeviceSessionViewModel();
        // 客户端 API 基座:登录后注入 token;操作许可门禁与使用日志上报共用它。需先于 Coordinator 创建。
        otaClient = new OtaApiClient();
        this.usageReporter = usageReporter ?? new UsageLogUploader(otaClient);
        // 操作许可门禁:生产环境每个用户操作运行前询问服务端(默认放行、封禁/停用拒绝);测试关闭。
        var gate = enableServerGate ? new ServerOperationGate(otaClient) : null;
        Coordinator = new OperationCoordinator(Session, LogService, notifyBlocked, gate, this.usageReporter);

        var deviceInfo = new DeviceInfoService(backend, cliRunner);
        var deviceSessionService = new DeviceSessionService(backend, deviceInfo, LogService);
        Monitor = new DeviceMonitorService(deviceSessionService, Session, Coordinator, logs: LogService);

        var toolPreferences = preferences ?? ToolPathPreferences.CreateDefault();
        var quickFlashService = new QuickFlashService(backend, cliRunner, LogService);
        // 发布瘦身:scrcpy/APK/payload_dumper 不再随包;统一走多镜像 failover 下载器按需获取。
        // 下载器与三个 provisioner 提升为字段,供登录后「组件安装」窗与软件页重开使用。
        assetDownloader = new RemoteAssetDownloader();
        payloadDumperProvisioner = new PayloadDumperProvisioner(
            assetDownloader,
            bundledExecutablePath: Path.Combine(AppContext.BaseDirectory, "payload-tools", "payload_dumper.exe"));
        var payloadDumper = new PayloadDumperRunner(payloadDumperProvisioner.ExecutablePath);
        scrcpyProvisioner = new ScrcpyProvisioningService(downloader: assetDownloader);
        mirrorService = new MirrorService(processRunner, LogService, provisioner: scrcpyProvisioner);
        rootResources = new VivoRootResourceService(AppContext.BaseDirectory, assetDownloader);
        var quickFlash = new QuickFlashViewModel(Session, quickFlashService, LogService, Coordinator);
        var firmwareExtract = new FirmwareExtractViewModel(
            LogService, payloadDumper, new VivoFirmwareExtractor(), provisioner: payloadDumperProvisioner);
        var mirror = new MirrorViewModel(Session, mirrorService, toolPreferences);
        var fileManager = new FileManagerViewModel(
            Session,
            new AdbFileService(backend, LogService),
            LogService,
            Coordinator,
            notifyError: message => MessageBox.Show(message, "奶蛙Flash", MessageBoxButton.OK, MessageBoxImage.Error));
        var lineFlash = new LineFlashViewModel(Session, new FastbootPartitionService(cliRunner), LogService);
        var fastbootPartitionTransport = new FastbootPartitionTransport(cliRunner);
        var adbExecutable = new PlatformToolsExecutableLocator(AppContext.BaseDirectory).Resolve("adb.exe");
        var adbRootPartitionTransport = new AdbRootPartitionTransport(
            new AdbRootTransferRunner(adbExecutable, new SystemAdbBinaryRunner()));
        var partitionWorkspace = new PartitionWorkspaceViewModel(
            Session,
            fastbootPartitionTransport,
            adbRootPartitionTransport,
            new PartitionExecutionService(
                Session,
                Coordinator,
                LogService,
                [fastbootPartitionTransport, adbRootPartitionTransport]),
            LogService,
            Coordinator,
            ConfirmPartitionOperation);
        var root = new RootViewModel(
            Session,
            quickFlashService,
            LogService,
            backend,
            rootResources,
            Coordinator);
        var overview = new OverviewViewModel(Session, backend, LogService, Coordinator);
        // 心跳与设备操作无关,独立于 OperationCoordinator 运行;强制退出回调处理「不打断刷写」。
        Heartbeat = new HeartbeatService(
            otaClient,
            onForceExitAsync: ForceExitAsync,
            onUpdateRequiredAsync: ShowUpdateRequiredAsync);
        Online = new OnlineViewModel(otaClient, Heartbeat);
        var safeFlash = new SafeFlashViewModel(
            Session,
            LogService,
            backend,
            otaClient,
            new OtaDownloadService(),
            new FirmwarePartitionExtractor(payloadDumper, payloadDumperProvisioner),
            Coordinator,
            cliRunner);

        MainViewModel = new MainViewModel(
            Session,
            overview,
            new OperationLogViewModel(LogService),
            deviceSessionService,
            quickFlash,
            mirror,
            fileManager,
            lineFlash,
            root,
            partitionWorkspace,
            firmwareExtract,
            safeFlash,
            Monitor,
            Coordinator,
            Online,
            new SoftwareViewModel(
                AppContext.BaseDirectory,
                preferences: toolPreferences,
                onReinstallDriver: () => new DriverReminderWindow(reinstallMode: true).ShowDialog(),
                provisioner: payloadDumperProvisioner,
                openResourceDownloader: () =>
                {
                    var viewModel = CreateResourceDownloadViewModel();
                    viewModel.Detect();
                    new ResourceDownloadWindow(viewModel).ShowDialog();
                }),
            onLogout: OnLogoutAsync);
        Monitor.DeviceRefreshed += MainViewModel.OnDeviceRefreshedAsync;

        firmwareExtract.SetFlashContinuation((image, partition) =>
        {
            quickFlash.PreparePatchedImage(image, partition);
            MainViewModel.SelectedPage = AppPage.FastbootFlash;
        });

        root.SetFlashContinuation((image, partition) =>
        {
            quickFlash.PreparePatchedImage(image, partition);
            MainViewModel.SelectedPage = AppPage.FastbootFlash;
        });
        root.SetVendorFlashContinuation((image, partition) =>
        {
            quickFlash.PreparePatchedImage(image, partition);
            MainViewModel.SelectedPage = AppPage.FastbootFlash;
        });
        lineFlash.SetQuickFlashContinuation((image, partition) =>
        {
            quickFlash.PreparePatchedImage(image, partition);
            MainViewModel.SelectedPage = AppPage.FastbootFlash;
        });
    }

    private static bool ConfirmPartitionOperation(string message) =>
        MessageBox.Show(
            message + Environment.NewLine + "请确认后开始执行，任务会在首个失败处分区停止。",
            "确认分区操作",
            MessageBoxButton.OKCancel,
            MessageBoxImage.Warning) == MessageBoxResult.OK;

    public DeviceSessionViewModel Session { get; }

    public OperationLogService LogService { get; }

    public OperationCoordinator Coordinator { get; }

    public DeviceMonitorService Monitor { get; }

    public MainViewModel MainViewModel { get; }

    /// <summary>在线会话心跳:登录后由 <see cref="StartSessionAsync"/> 启动。</summary>
    public HeartbeatService Heartbeat { get; }

    /// <summary>在线状态页:登录后由 <see cref="StartSessionAsync"/> 启动轮询。</summary>
    public OnlineViewModel Online { get; }

    /// <summary>客户端使用日志上报器:OperationCoordinator 记录 → 批量上传(30s 定时 + 退出 flush)。</summary>
    public UsageLogUploader UsageReporter => usageReporter;

    /// <summary>用户点击登出,优雅下线完成后触发;App 据此关主窗回登录窗。</summary>
    public event EventHandler? LogoutRequested;

    /// <summary>本次启动的会话 id(客户端生成,GUID)。强制下线/在线列表以此标记「自己」。</summary>
    public string? SessionId { get; private set; }

    /// <summary>当前登录用户的显示名(登录接口返回)。</summary>
    public string? CurrentUsername { get; private set; }

    /// <summary>登录后注入 API token,后续请求带 Authorization:Bearer。</summary>
    public void SetAuthToken(string token) => otaClient.Token = token;

    /// <summary>构建「组件安装」窗 VM(登录后检测 + 软件页重开共用同一套 provisioners)。</summary>
    public ResourceDownloadViewModel CreateResourceDownloadViewModel() =>
        new(scrcpyProvisioner, payloadDumperProvisioner, rootResources);

    /// <summary>
    /// 登录成功后启动在线会话:设 token、生成会话 id、启动心跳与在线状态轮询。
    /// 心跳会在本实例剩余生命周期内持续;App 退出时由 <see cref="StopAsync"/> 收尾(goodbye)。
    /// 幂等:重复调用不再重新生成会话 id(运行中的心跳仍用旧 id,避免公开 id 与真实会话脱钩)。
    /// </summary>
    public void StartSessionAsync(string token, string username)
    {
        SetAuthToken(token);
        CurrentUsername = username;
        MainViewModel.AccountName = username;
        if (sessionStarted)
        {
            return;
        }

        sessionStarted = true;
        SessionId = Guid.NewGuid().ToString("N");
        Heartbeat.Start(SessionId);
        Online.Start();
        usageReporter.Start();
    }

    private bool sessionStarted;

    public static AppComposition CreateDefault() => new(
        FastbootRsApiFactory.CreateDefault(),
        new SystemProcessRunner(),
        notifyBlocked: message => MessageBox.Show(message, "奶蛙Flash", MessageBoxButton.OK, MessageBoxImage.Information),
        operationsLogPath: Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "VivoKsu",
            "operations.log"));

    public static AppComposition CreateForTesting(IFastbootRsNativeApi nativeApi, IProcessRunner processRunner) =>
        new(
            nativeApi,
            processRunner,
            new ToolPathPreferences(Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"{Guid.NewGuid():N}.json")),
            notifyBlocked: _ => { },
            enableServerGate: false);

    public Task StartAsync(CancellationToken cancellationToken = default) => Monitor.StartAsync(cancellationToken);

    public async Task StopAsync()
    {
        if (stopped)
        {
            return;
        }

        stopped = true;
        MainViewModel.StopClock();
        Monitor.DeviceRefreshed -= MainViewModel.OnDeviceRefreshedAsync;
        // 先停心跳(尽早发 goodbye,让服务端立即标记离线),再停 Monitor——否则 Monitor 的
        // DrainRefreshesAsync 可能烧掉 OnExit 的 5s 预算,goodbye 连机会都没有。
        Online.Stop();
        await Heartbeat.StopAsync();
        await Monitor.StopAsync();
        await mirrorService.StopAsync();
        await Monitor.DisposeAsync();
        Coordinator.Dispose();

        // 退出前把缓冲的使用日志上传到服务端(best-effort,失败不影响退出)。
        try
        {
            await usageReporter.FlushAsync();
        }
        catch
        {
            // 离线时丢弃;不阻塞退出。
        }

        usageReporter.Dispose();
        // 清理可达数 GB 的临时文件(URL 下载 gzip + 各盘 safe-flash staging)在后台线程跑:
        // 登出路径 StopAsync 在 UI 线程 await,同步删盘会冻结界面几秒。force-exit 路径
        // (FinishForceExit)保留同步——进程随即退出,无可见冻结。
        await Task.Run(CleanupTemporaryFiles);
    }

    private async Task OnLogoutAsync()
    {
        // 优雅下线(心跳 goodbye / 使用日志 flush / 停设备监视),完成后通知 App 回登录窗。
        await StopAsync();
        LogoutRequested?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>清理可达数 GB 的临时文件(URL 下载 gzip;安全刷写 staging 成功时已由页面清理)。正常退出与强制退出共用。</summary>
    private static void CleanupTemporaryFiles()
    {
        try
        {
            var downloadedDirectory = VivoFirmwareExtractor.DownloadedGzipDirectory;
            if (Directory.Exists(downloadedDirectory))
            {
                Directory.Delete(downloadedDirectory, recursive: true);
            }
        }
        catch
        {
            // Best effort; Windows 磁盘清理最终会回收临时目录。
        }

        // 不再清空 {盘}\VivoKsu\safe-flash:设备断开/失败时解包好的镜像保留在该处,
        // 供「VIVO 线刷 → 选择解包文件夹」跨重启复用。成功路径已由 SafeFlashViewModel 自行
        // CleanupStaging;失败路径刻意保留。残留可由用户手动清理。
    }

    private string? pendingForceExitReason;
    private int forceExitCompleting;

    /// <summary>
    /// 服务端强制退出(心跳返回 force_exit / 401 / 403)。先停心跳并发 goodbye;
    /// 随后在 UI 线程处理——刷写/下载中绝不直接杀进程(会把设备留在半刷状态):
    /// 取消当前操作并等 Idle 后再退出;空闲则立即优雅退出。
    /// </summary>
    private async Task ForceExitAsync(string reason)
    {
        // 只发 goodbye(不 StopAsync——StopAsync 会等待心跳循环,而本回调正由循环内调用,
        // 自己等自己会死锁;循环在回调返回后自行结束)。
        await Heartbeat.SendGoodbyeAsync().ConfigureAwait(false);

        var dispatcher = System.Windows.Application.Current?.Dispatcher;
        if (dispatcher is null || dispatcher.CheckAccess())
        {
            CompleteForceExit(reason);
            return;
        }

        await dispatcher.InvokeAsync(() => CompleteForceExit(reason)).Task.ConfigureAwait(false);
    }

    private void CompleteForceExit(string reason)
    {
        // 先订阅再判忙,避免「判忙后、订阅前」操作完成的竞态漏掉 StateChanged。
        Coordinator.StateChanged += OnPendingExitStateChanged;
        if (!Coordinator.IsBusy)
        {
            FinishForceExit();
            return;
        }

        pendingForceExitReason ??= reason;
        Coordinator.CancelCurrent();
        MessageBox.Show(
            $"服务端要求退出: {reason}\n\n正在结束当前操作,完成后自动退出。",
            "奶蛙Flash",
            MessageBoxButton.OK,
            MessageBoxImage.Information);
        // 用户关闭弹窗后再查一次(弥补消息丢失窗口);仍未结束则由 StateChanged 接管。
        if (!Coordinator.IsBusy)
        {
            FinishForceExit();
        }
    }

    private void OnPendingExitStateChanged(object? sender, EventArgs e)
    {
        if (!Coordinator.IsBusy)
        {
            FinishForceExit();
        }
    }

    private void FinishForceExit()
    {
        if (Interlocked.Exchange(ref forceExitCompleting, 1) != 0)
        {
            return;
        }

        Coordinator.StateChanged -= OnPendingExitStateChanged;
        CleanupTemporaryFiles();
        Environment.Exit(0);
    }

    /// <summary>服务端 426 强制更新(心跳路径):先发 goodbye,再在 UI 线程弹更新窗并退出。</summary>
    private async Task ShowUpdateRequiredAsync(UpdateRequiredException update)
    {
        await Heartbeat.SendGoodbyeAsync().ConfigureAwait(false);
        var app = System.Windows.Application.Current;
        if (app is null)
        {
            return;
        }

        await app.Dispatcher.InvokeAsync(() =>
        {
            new UpdateRequiredWindow(update.Latest, update.MinVersion, update.DownloadUrl).ShowDialog();
            app.Shutdown();
        }).Task.ConfigureAwait(false);
    }
}
