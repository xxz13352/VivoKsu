using System.IO;
using System.Net.Http;
using System.Windows;
using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class AppComposition
{
    private readonly MirrorService mirrorService;
    private bool stopped;

    private AppComposition(
        IFastbootRsNativeApi nativeApi,
        IProcessRunner processRunner,
        ToolPathPreferences? preferences = null)
    {
        var backend = new FastbootRsBackend(nativeApi);
        LogService = new OperationLogService();
        Session = new DeviceSessionViewModel();
        Coordinator = new OperationCoordinator(Session, LogService);

        var deviceInfo = new DeviceInfoService(backend);
        var deviceSessionService = new DeviceSessionService(backend, deviceInfo, LogService);
        Monitor = new DeviceMonitorService(deviceSessionService, Session, Coordinator, logs: LogService);

        var toolPreferences = preferences ?? ToolPathPreferences.CreateDefault();
        var quickFlashService = new QuickFlashService(backend, LogService);
        var payloadDumper = new PayloadDumperRunner(
            Path.Combine(AppContext.BaseDirectory, "payload-tools", "payload_dumper.exe"));
        mirrorService = new MirrorService(processRunner, LogService, provisioner: new ScrcpyProvisioningService());
        var quickFlash = new QuickFlashViewModel(Session, quickFlashService, LogService, Coordinator);
        var firmwareExtract = new FirmwareExtractViewModel(LogService, payloadDumper, new VivoFirmwareExtractor());
        var mirror = new MirrorViewModel(Session, mirrorService, toolPreferences);
        var fileManager = new FileManagerViewModel(Session, new AdbFileService(backend, LogService), LogService, Coordinator);
        var lineFlash = new LineFlashViewModel(Session, new FastbootPartitionService(backend), LogService);
        var fastbootPartitionTransport = new FastbootPartitionTransport(backend);
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
            new VivoRootResourceService(AppContext.BaseDirectory),
            Coordinator);
        var overview = new OverviewViewModel(Session, backend, LogService, Coordinator);
        var fastbootRsCli = new FastbootRsCliRunner(
            Path.Combine(AppContext.BaseDirectory, "platform-tools", "fastboot-rs.exe"));
        var safeFlash = new SafeFlashViewModel(
            Session,
            LogService,
            backend,
            new OtaApiClient(),
            new OtaDownloadService(),
            new FirmwarePartitionExtractor(payloadDumper),
            Coordinator,
            fastbootRsCli);

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
            Coordinator);
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

    public static AppComposition CreateDefault() => new(
        FastbootRsApiFactory.CreateDefault(),
        new SystemProcessRunner());

    public static AppComposition CreateForTesting(IFastbootRsNativeApi nativeApi, IProcessRunner processRunner) =>
        new(
            nativeApi,
            processRunner,
            new ToolPathPreferences(Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", $"{Guid.NewGuid():N}.json")));

    public Task StartAsync(CancellationToken cancellationToken = default) => Monitor.StartAsync(cancellationToken);

    public async Task StopAsync()
    {
        if (stopped)
        {
            return;
        }

        stopped = true;
        Monitor.DeviceRefreshed -= MainViewModel.OnDeviceRefreshedAsync;
        await Monitor.StopAsync();
        await mirrorService.StopAsync();
        await Monitor.DisposeAsync();
        Coordinator.Dispose();
    }
}
