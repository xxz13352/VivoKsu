using System.ComponentModel;
using System.IO;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Microsoft.Win32;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public partial class RootViewModel : ObservableObject
{
    /// <summary>全自动 ROOT 等待设备进入 fastboot 的超时;Root 页没有手动取消入口,必须有时限。</summary>
    private static readonly TimeSpan FastbootWaitTimeout = TimeSpan.FromSeconds(180);

    private readonly DeviceSessionViewModel session;
    private readonly QuickFlashService imageInspector;
    private readonly OperationLogService logs;
    private readonly FastbootRsBackend? backend;
    private readonly VivoRootResourceService resources;
    private readonly RootPatchArtifactService artifacts;
    private readonly VivoKsuDevicePatchService? vivoPatcher;
    private readonly VivoVendorBootProcessor? vendorBootProcessor;
    private readonly IOperationCoordinator? coordinator;
    private Action<FlashImageInfo, QuickFlashPartition>? flashContinuation;
    private Action<FlashImageInfo, QuickFlashPartition>? vendorFlashContinuation;

    [ObservableProperty]
    private FlashImageInfo? selectedImage;

    [ObservableProperty]
    private FlashImageInfo? selectedVendorImage;

    [ObservableProperty]
    private bool isInspectingImage;

    [ObservableProperty]
    private FlashImageInfo? patchedImage;

    [ObservableProperty]
    private FlashImageInfo? patchedVendorImage;

    [ObservableProperty]
    private bool isVivoKsuSelected = true;

    [ObservableProperty]
    private string selectedKmi = "android14-6.1";

    [ObservableProperty]
    private bool useAutomaticKmi = true;

    public RootViewModel(
        DeviceSessionViewModel session,
        QuickFlashService imageInspector,
        OperationLogService logs)
        : this(session, imageInspector, logs, null, null, null, null)
    {
    }

    public RootViewModel(
        DeviceSessionViewModel session,
        QuickFlashService imageInspector,
        OperationLogService logs,
        FastbootRsBackend? backend,
        VivoRootResourceService? resources,
        IOperationCoordinator? coordinator = null,
        RootPatchArtifactService? artifacts = null)
    {
        this.session = session;
        this.imageInspector = imageInspector;
        this.logs = logs;
        this.backend = backend;
        this.resources = resources ?? new VivoRootResourceService(AppContext.BaseDirectory);
        this.artifacts = artifacts ?? new RootPatchArtifactService();
        this.coordinator = coordinator;
        vivoPatcher = backend is null ? null : new VivoKsuDevicePatchService(backend, this.resources, imageInspector);
        vendorBootProcessor = backend is null ? null : new VivoVendorBootProcessor(backend, this.resources, imageInspector);

        BrowseImageCommand = new AsyncRelayCommand(BrowseImageAsync);
        BrowseVendorImageCommand = new AsyncRelayCommand(BrowseVendorImageAsync);
        PatchImageCommand = new AsyncRelayCommand(PatchImageAsync, CanPatchImage);
        ContinueToFlashCommand = new RelayCommand(ContinueToFlash, () => PatchedImage is not null);
        ContinueVendorToFlashCommand = new RelayCommand(ContinueVendorToFlash, () => PatchedVendorImage is not null);
        InstallManagerCommand = new AsyncRelayCommand(InstallManagerAsync, CanInstallManager);
        ResolveKmiCommand = new AsyncRelayCommand(ResolveKmiAsync, CanResolveKmi);
        RunAutomaticRootCommand = new AsyncRelayCommand(RunAutomaticRootAsync, CanRunAutomaticRoot);
        StopCommand = new RelayCommand(Stop, CanStop);
        if (coordinator is not null)
        {
            coordinator.StateChanged += OnCoordinatorStateChanged;
        }

        session.PropertyChanged += OnSessionPropertyChanged;
    }

    public IAsyncRelayCommand BrowseImageCommand { get; }

    public IAsyncRelayCommand BrowseVendorImageCommand { get; }

    public IAsyncRelayCommand PatchImageCommand { get; }

    public IRelayCommand ContinueToFlashCommand { get; }

    public IRelayCommand ContinueVendorToFlashCommand { get; }

    public IAsyncRelayCommand InstallManagerCommand { get; }

    public IAsyncRelayCommand ResolveKmiCommand { get; }

    public IAsyncRelayCommand RunAutomaticRootCommand { get; }

    public IRelayCommand StopCommand { get; }

    public IOperationCoordinator? Coordinator => coordinator;

    private bool CanStop() => coordinator?.IsBusy == true;

    private void Stop()
    {
        // 全自动 ROOT 等待 fastboot / 刷写期间,用户可从此页取消,释放 operationGate。
        coordinator?.CancelCurrent();
    }

    private void OnCoordinatorStateChanged(object? sender, EventArgs eventArgs) => StopCommand.NotifyCanExecuteChanged();

    public IReadOnlyList<string> KmiOptions => VivoRootResourceService.SupportedKmis;

    public IReadOnlyList<string> ManagerOptions => ["Vivo KSU", "官方 KernelSU"];

    public string SelectedManagerKey => IsVivoKsuSelected ? "KSU" : "OfficialKsu";

    public string EffectiveKmi => UseAutomaticKmi ? DetectedKmi : SelectedKmi;

    public bool IsOfficialKsuSelected
    {
        get => !IsVivoKsuSelected;
        set
        {
            if (value)
            {
                IsVivoKsuSelected = false;
            }
        }
    }

    public string SelectedManagerLabel => IsVivoKsuSelected ? "Vivo KSU" : "官方 KernelSU";

    public string ManagerApkName => TryResolveManager()?.ApkPath is { } path ? Path.GetFileName(path) : "未找到管理器 APK";

    public bool IsManagerApkAvailable
    {
        get
        {
            try
            {
                resources.VerifyManagerApk(resources.ResolveManager(SelectedManagerKey));
                return true;
            }
            catch (Exception)
            {
                return false;
            }
        }
    }

    public string ManagerApkStatus => IsManagerApkAvailable
        ? $"{SelectedManagerLabel} APK 已校验"
        : $"{SelectedManagerLabel} APK 缺失或校验失败";

    public string DetectedKmi
    {
        get
        {
            var release = session.Details.KernelVersion;
            try
            {
                return string.IsNullOrWhiteSpace(release) || release is "--" or "Not available"
                    ? "未检测"
                    : VivoRootResourceService.MapKernelRelease(release);
            }
            catch (ArgumentException)
            {
                return "未识别";
            }
        }
    }

    public string ImageName => SelectedImage is null ? "未选择 init_boot 镜像" : Path.GetFileName(SelectedImage.Path);

    public string ImageSizeDisplay => SelectedImage is null ? "--" : FormatBytes(SelectedImage.SizeBytes);

    public string VendorImageName => SelectedVendorImage is null ? "未选择 vendor_boot 镜像" : Path.GetFileName(SelectedVendorImage.Path);

    public string VendorImageSizeDisplay => SelectedVendorImage is null ? "--" : FormatBytes(SelectedVendorImage.SizeBytes);

    public string PatchedImageName => PatchedImage is null ? "尚未生成 init_boot 修补镜像" : Path.GetFileName(PatchedImage.Path);

    public string PatchedVendorImageName => PatchedVendorImage is null ? "尚未生成 vendor_boot 修补镜像" : Path.GetFileName(PatchedVendorImage.Path);

    public bool IsReadyForVivoPatch =>
        IsVivoKsuSelected &&
        SelectedImage is not null &&
        IsInitBootImage &&
        session.IsAdbConnected &&
        vivoPatcher is not null &&
        IsManagerApkAvailable &&
        KmiOptions.Contains(EffectiveKmi, StringComparer.Ordinal);

    public bool IsReadyForOfficialKsu =>
        IsOfficialKsuSelected &&
        (IsInitBootImage || IsVendorBootImage) &&
        session.IsAdbConnected &&
        vivoPatcher is not null &&
        vendorBootProcessor is not null &&
        IsManagerApkAvailable &&
        KmiOptions.Contains(EffectiveKmi, StringComparer.Ordinal);

    public bool IsReadyForPatch => IsReadyForVivoPatch || IsReadyForOfficialKsu;

    public bool IsReadyForAutomaticRoot => IsVivoKsuSelected
        ? IsReadyForVivoPatch
        : IsReadyForOfficialKsu && IsInitBootImage && IsVendorBootImage;

    public string PreflightSummary
    {
        get
        {
            if (SelectedImage is null && SelectedVendorImage is null)
            {
                return IsOfficialKsuSelected ? "等待选择 init_boot 或 vendor_boot 镜像" : "等待选择启动镜像";
            }

            if (!session.IsAdbConnected)
            {
                return "等待 ADB 设备";
            }

            if (!IsManagerApkAvailable)
            {
                return ManagerApkStatus;
            }

            if (SelectedImage is not null && !IsInitBootImage)
            {
                return "ROOT 镜像必须为 .img 或 .bin 格式";
            }

            if (IsOfficialKsuSelected && SelectedVendorImage is not null && !IsVendorBootImage)
            {
                return "ROOT 镜像必须为 .img 或 .bin 格式";
            }

            if (!IsReadyForPatch)
            {
                return "等待 KMI 与设备状态";
            }

            return IsOfficialKsuSelected && !IsReadyForAutomaticRoot
                ? "官版 KSU 已就绪，可修补已选择镜像；全自动需要两份镜像"
                : "ROOT 前置条件已就绪";
        }
    }

    partial void OnSelectedImageChanged(FlashImageInfo? value)
    {
        PatchedImage = null;
        NotifyPreflightChanged();
    }

    partial void OnSelectedVendorImageChanged(FlashImageInfo? value)
    {
        PatchedVendorImage = null;
        NotifyPreflightChanged();
    }

    partial void OnPatchedImageChanged(FlashImageInfo? value)
    {
        OnPropertyChanged(nameof(PatchedImageName));
        ContinueToFlashCommand.NotifyCanExecuteChanged();
    }

    partial void OnPatchedVendorImageChanged(FlashImageInfo? value)
    {
        OnPropertyChanged(nameof(PatchedVendorImageName));
        ContinueVendorToFlashCommand.NotifyCanExecuteChanged();
    }

    partial void OnIsVivoKsuSelectedChanged(bool value)
    {
        // A patch produced for the previous manager variant is no longer valid.
        PatchedImage = null;
        if (value)
        {
            SelectedVendorImage = null;
            PatchedVendorImage = null;
        }

        OnPropertyChanged(nameof(IsOfficialKsuSelected));
        OnPropertyChanged(nameof(SelectedManagerKey));
        NotifyRootOptionsChanged();
        NotifyPreflightChanged();
    }

    partial void OnSelectedKmiChanged(string value)
    {
        PatchedImage = null;
        NotifyKmiChanged();
    }

    partial void OnUseAutomaticKmiChanged(bool value)
    {
        PatchedImage = null;
        NotifyKmiChanged();
    }

    public void SetFlashContinuation(Action<FlashImageInfo, QuickFlashPartition> continuation) => flashContinuation = continuation;

    public void SetVendorFlashContinuation(Action<FlashImageInfo, QuickFlashPartition> continuation) => vendorFlashContinuation = continuation;

    private async Task BrowseImageAsync() => await BrowseImageAsync(false);

    private async Task BrowseVendorImageAsync() => await BrowseImageAsync(true);

    private async Task BrowseImageAsync(bool vendorBoot)
    {
        var dialog = new OpenFileDialog
        {
            Filter = "Android image (*.img;*.bin)|*.img;*.bin",
            CheckFileExists = true,
            Multiselect = false,
            Title = vendorBoot ? "选择 vendor_boot 镜像" : "选择 init_boot 镜像"
        };

        if (dialog.ShowDialog() != true)
        {
            return;
        }

        IsInspectingImage = true;
        var imageKind = vendorBoot ? "vendor_boot" : "init_boot";
        var title = $"正在读取 {imageKind} 镜像";

        if (coordinator is not null)
        {
            try
            {
                await RunCoordinatedAsync(OperationKind.Hashing, title, async (context, cancellationToken) =>
                {
                    context.ReportStage($"正在读取镜像: {Path.GetFileName(dialog.FileName)}");
                    var image = await imageInspector.InspectImageAsync(dialog.FileName, cancellationToken);
                    if (vendorBoot)
                    {
                        SelectedVendorImage = image;
                    }
                    else
                    {
                        SelectedImage = image;
                    }

                    context.ReportStage("ROOT 镜像读取完成");
                });
            }
            finally
            {
                IsInspectingImage = false;
            }

            return;
        }

        session.BeginOperation(OperationKind.Hashing, $"正在读取 {(vendorBoot ? "vendor_boot" : "init_boot")} 镜像");
        logs.Write(OperationLogLevel.Info, $"正在读取镜像: {Path.GetFileName(dialog.FileName)}。 ");

        try
        {
            var image = await imageInspector.InspectImageAsync(dialog.FileName, CancellationToken.None);
            if (vendorBoot)
            {
                SelectedVendorImage = image;
            }
            else
            {
                SelectedImage = image;
            }

            session.CompleteOperation("ROOT 镜像读取完成");
            logs.Write(OperationLogLevel.Success, "ROOT 镜像读取完成。 ");
        }
        catch (Exception exception)
        {
            session.FailOperation("ROOT 镜像读取失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
        finally
        {
            IsInspectingImage = false;
        }
    }

    private bool CanPatchImage() => IsReadyForPatch;

    private bool CanInstallManager() => backend is not null && session.IsAdbConnected && !session.IsBusy && IsManagerApkAvailable;

    private async Task InstallManagerAsync()
    {
        if (!CanInstallManager() || backend is null)
        {
            return;
        }

        var manager = resources.ResolveManager(SelectedManagerKey);

        if (coordinator is not null)
        {
            try
            {
                await RunCoordinatedAsync(OperationKind.Installing, $"正在安装 {SelectedManagerLabel} 管理器", async (context, cancellationToken) =>
                {
                    context.ReportStage($"正在安装 {SelectedManagerLabel}: {ManagerApkName}");
                    await InstallManagerCoreAsync(cancellationToken, context);
                    context.ReportStage($"{SelectedManagerLabel} 管理器已安装并启动");
                });
            }
            finally
            {
                NotifyRootOptionsChanged();
            }

            return;
        }

        session.BeginOperation(OperationKind.Installing, $"正在安装 {SelectedManagerLabel} 管理器");
        logs.Write(OperationLogLevel.Info, $"正在安装 {SelectedManagerLabel}: {ManagerApkName}。 ");

        try
        {
            await InstallManagerCoreAsync(CancellationToken.None);
            session.CompleteOperation($"{SelectedManagerLabel} 管理器已安装并启动");
            logs.Write(OperationLogLevel.Success, $"{SelectedManagerLabel} 管理器已安装并启动。 ");
        }
        catch (Exception exception)
        {
            session.FailOperation("ROOT 管理器安装失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
        finally
        {
            NotifyRootOptionsChanged();
        }
    }

    private async Task PatchImageAsync()
    {
        if (!CanPatchImage() || vivoPatcher is null)
        {
            return;
        }

        if (coordinator is not null)
        {
            await RunCoordinatedAsync(OperationKind.Hashing, "正在修补 ROOT 镜像", async (context, cancellationToken) =>
            {
                context.ReportStage($"正在修补 {SelectedManagerLabel} 镜像");
                await PatchImagesCoreAsync(cancellationToken, context);
                context.ReportStage(GetPatchCompletionMessage());
            });
            return;
        }

        session.BeginOperation(OperationKind.Hashing, "正在修补 ROOT 镜像");
        logs.Write(OperationLogLevel.Info, $"正在修补 {SelectedManagerLabel} 镜像。 ");

        try
        {
            await PatchImagesCoreAsync(CancellationToken.None);

            session.CompleteOperation(GetPatchCompletionMessage());
            logs.Write(OperationLogLevel.Success, "ROOT 镜像修补完成，等待移交快速刷写。 ");
        }
        catch (Exception exception)
        {
            session.FailOperation("ROOT 镜像修补失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
    }

    private void ContinueToFlash()
    {
        if (PatchedImage is not null)
        {
            flashContinuation?.Invoke(PatchedImage, QuickFlashPartition.InitBoot);
        }
    }

    private void ContinueVendorToFlash()
    {
        if (PatchedVendorImage is not null)
        {
            vendorFlashContinuation?.Invoke(PatchedVendorImage, QuickFlashPartition.VendorBoot);
        }
    }

    private async Task ResolveKmiAsync()
    {
        if (!CanResolveKmi() || backend is null)
        {
            return;
        }

        if (coordinator is not null)
        {
            try
            {
                await RunCoordinatedAsync(OperationKind.Discovering, "正在识别设备 KMI", async (context, cancellationToken) =>
                {
                    context.ReportStage("正在读取设备内核版本");
                    var kernelVersion = (await backend.ShellAsync(session.Serial, "uname -r", cancellationToken)).Trim();
                    if (string.IsNullOrWhiteSpace(kernelVersion))
                    {
                        throw new InvalidDataException("设备未返回内核版本，无法识别 KMI。 ");
                    }

                    session.ApplyDetails(session.Details with { KernelVersion = kernelVersion });
                    context.ReportStage($"已识别 KMI: {DetectedKmi}");
                });
            }
            finally
            {
                NotifyKmiChanged();
            }

            return;
        }

        session.BeginOperation(OperationKind.Discovering, "正在识别设备 KMI");
        try
        {
            var kernelVersion = (await backend.ShellAsync(session.Serial, "uname -r", CancellationToken.None)).Trim();
            if (string.IsNullOrWhiteSpace(kernelVersion))
            {
                throw new InvalidDataException("设备未返回内核版本，无法识别 KMI。 ");
            }

            session.ApplyDetails(session.Details with { KernelVersion = kernelVersion });
            session.CompleteOperation($"已识别 KMI: {DetectedKmi}");
            logs.Write(OperationLogLevel.Success, $"设备内核 {kernelVersion} 已映射为 {DetectedKmi}。 ");
        }
        catch (Exception exception)
        {
            session.FailOperation("KMI 自动识别失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
        finally
        {
            NotifyKmiChanged();
        }
    }

    private async Task RunAutomaticRootAsync()
    {
        if (!CanRunAutomaticRoot() || backend is null)
        {
            return;
        }

        if (coordinator is not null)
        {
            try
            {
                await RunCoordinatedAsync(OperationKind.Installing, "ROOT 自动流程", async (context, cancellationToken) =>
                {
                    context.ReportStage($"ROOT 自动流程: 正在安装 {SelectedManagerLabel}", OperationKind.Installing);
                    await InstallManagerCoreAsync(cancellationToken, context);

                    context.ReportStage("ROOT 自动流程: 正在修补镜像", OperationKind.Hashing);
                    await PatchImagesCoreAsync(cancellationToken, context);

                    var images = BuildRootFlashImages();
                    // VIVO root 必须进 fastbootd(adb reboot fastboot),bootloader 模式刷不了启动分区。
                    context.ReportStage("ROOT 自动流程: 正在重启至 fastbootd", OperationKind.Rebooting);
                    await backend.RebootAsync(session.Serial, "fastboot", cancellationToken);

                    context.ReportStage("ROOT 自动流程: 正在等待并刷写 ROOT 镜像", OperationKind.Flashing);
                    await imageInspector.FlashRootImagesAsync(
                        session, images, FastbootTarget.Fastbootd, cancellationToken, context,
                        waitTimeout: FastbootWaitTimeout, expectedSerial: session.Serial);
                });
            }
            finally
            {
                NotifyRootOptionsChanged();
                NotifyPreflightChanged();
            }

            return;
        }

        try
        {
            session.BeginOperation(OperationKind.Installing, $"ROOT 自动流程: 正在安装 {SelectedManagerLabel}");
            logs.Write(OperationLogLevel.Info, $"ROOT 自动流程开始: {SelectedManagerLabel}，KMI {EffectiveKmi}。 ");
            await InstallManagerCoreAsync(CancellationToken.None);

            session.BeginOperation(OperationKind.Hashing, "ROOT 自动流程: 正在修补镜像");
            await PatchImagesCoreAsync(CancellationToken.None);

            var images = BuildRootFlashImages();

            session.BeginOperation(OperationKind.Rebooting, "ROOT 自动流程: 正在重启至 fastbootd");
            logs.Write(OperationLogLevel.Info, "ROOT 自动流程正在重启设备至 fastbootd。 ");
            await backend.RebootAsync(session.Serial, "fastboot", CancellationToken.None);
            await imageInspector.FlashRootImagesAsync(
                session, images, FastbootTarget.Fastbootd, CancellationToken.None,
                waitTimeout: FastbootWaitTimeout, expectedSerial: session.Serial);
        }
        catch (Exception exception)
        {
            session.FailOperation("ROOT 全自动流程失败");
            logs.Write(OperationLogLevel.Error, exception.Message);
        }
        finally
        {
            NotifyRootOptionsChanged();
            NotifyPreflightChanged();
        }
    }

    private async Task InstallManagerCoreAsync(CancellationToken cancellationToken, OperationContext? context = null)
    {
        if (backend is null)
        {
            throw new InvalidOperationException("未初始化 Fastboot 后端。 ");
        }

        var manager = resources.ResolveManager(SelectedManagerKey);
        context?.ReportStage($"正在安装 {SelectedManagerLabel} 管理器", OperationKind.Installing);
        await backend.InstallAsync(session.Serial, manager.ApkPath, replace: true, cancellationToken);
        context?.ReportStage("正在验证 ROOT 管理器安装状态", OperationKind.Installing);
        var packageState = await backend.ShellAsync(session.Serial, $"pm path {manager.PackageName}", cancellationToken);
        if (!packageState.Contains("package:", StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidOperationException($"未检测到已安装的管理器包: {manager.PackageName}");
        }

        context?.ReportStage("正在启动 ROOT 管理器", OperationKind.Installing);
        await backend.ShellAsync(session.Serial, $"am start -n {manager.PackageName}/{manager.ActivityName}", cancellationToken);
    }

    private async Task PatchImagesCoreAsync(CancellationToken cancellationToken, OperationContext? context = null)
    {
        if (vivoPatcher is null)
        {
            throw new InvalidOperationException("未初始化 ROOT 修补服务。 ");
        }

        var patchedImages = new List<FlashImageInfo>(2);
        if (SelectedImage is not null)
        {
            if (!IsInitBootImage)
            {
                throw new InvalidDataException("ROOT 镜像必须为 .img 或 .bin 格式。 ");
            }

            PatchedImage ??= await vivoPatcher.PatchAsync(
                session.Serial,
                SelectedManagerKey,
                EffectiveKmi,
                SelectedImage,
                cancellationToken,
                context);
            patchedImages.Add(PatchedImage);
        }

        if (IsOfficialKsuSelected && SelectedVendorImage is not null)
        {
            if (!IsVendorBootImage)
            {
                throw new InvalidDataException("ROOT 镜像必须为 .img 或 .bin 格式。 ");
            }

            if (vendorBootProcessor is null)
            {
                throw new InvalidOperationException("未初始化 vendor_boot 修补服务。 ");
            }

            PatchedVendorImage ??= await vendorBootProcessor.PatchAsync(
                session.Serial,
                SelectedVendorImage,
                cancellationToken,
                context);
            patchedImages.Add(PatchedVendorImage);
        }

        if (patchedImages.Count == 0)
        {
            throw new InvalidOperationException("至少选择一个 ROOT 镜像后再修补。 ");
        }

        var exported = artifacts.ExportToDesktop(patchedImages);
        var exportedIndex = 0;
        if (PatchedImage is not null)
        {
            PatchedImage = exported[exportedIndex++];
        }

        if (PatchedVendorImage is not null)
        {
            PatchedVendorImage = exported[exportedIndex];
        }

        context?.ReportStage($"修补镜像已保存到桌面\\{RootPatchArtifactService.OutputFolderName}");
    }

    private IReadOnlyList<(QuickFlashPartition Partition, FlashImageInfo Image)> BuildRootFlashImages()
    {
        var images = new List<(QuickFlashPartition Partition, FlashImageInfo Image)>();
        if (PatchedImage is not null)
        {
            images.Add((QuickFlashPartition.InitBoot, PatchedImage));
        }

        if (IsOfficialKsuSelected && PatchedVendorImage is not null)
        {
            images.Add((QuickFlashPartition.VendorBoot, PatchedVendorImage));
        }

        if (images.Count == 0)
        {
            throw new InvalidOperationException("未生成 ROOT 修补镜像。 ");
        }

        return images;
    }

    private string GetPatchCompletionMessage() => IsOfficialKsuSelected
        ? PatchedImage is not null && PatchedVendorImage is not null
            ? "官方 KSU 双镜像修补完成"
            : "官方 KSU 已修补所选镜像"
        : "Vivo KSU init_boot 修补完成";

    private async Task RunCoordinatedAsync(
        OperationKind kind,
        string title,
        Func<OperationContext, CancellationToken, Task> operation)
    {
        try
        {
            await coordinator!.RunAsync(kind, title, operation);
        }
        catch (OperationCanceledException)
        {
            // The coordinator has already updated the shared terminal state and log.
        }
        catch (Exception)
        {
            // The coordinator has already updated the shared terminal state and log.
        }
    }

    private VivoRootManagerResource? TryResolveManager()
    {
        try
        {
            return resources.ResolveManager(SelectedManagerKey);
        }
        catch (ArgumentException)
        {
            return null;
        }
    }

    private void OnSessionPropertyChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName is nameof(DeviceSessionViewModel.ConnectionState) or nameof(DeviceSessionViewModel.Details) or nameof(DeviceSessionViewModel.IsBusy))
        {
            // A device change or fresh details can alter the detected KMI, which makes
            // a previously built patch invalid; IsBusy transitions must not do so.
            if (args.PropertyName is nameof(DeviceSessionViewModel.ConnectionState) or nameof(DeviceSessionViewModel.Details))
            {
                PatchedImage = null;
            }

            NotifyRootOptionsChanged();
            NotifyKmiChanged();
            NotifyPreflightChanged();
        }
    }

    private void NotifyRootOptionsChanged()
    {
        OnPropertyChanged(nameof(SelectedManagerLabel));
        OnPropertyChanged(nameof(ManagerApkName));
        OnPropertyChanged(nameof(IsManagerApkAvailable));
        OnPropertyChanged(nameof(ManagerApkStatus));
        OnPropertyChanged(nameof(DetectedKmi));
        InstallManagerCommand.NotifyCanExecuteChanged();
        ResolveKmiCommand.NotifyCanExecuteChanged();
        RunAutomaticRootCommand.NotifyCanExecuteChanged();
    }

    private void NotifyPreflightChanged()
    {
        OnPropertyChanged(nameof(ImageName));
        OnPropertyChanged(nameof(ImageSizeDisplay));
        OnPropertyChanged(nameof(VendorImageName));
        OnPropertyChanged(nameof(VendorImageSizeDisplay));
        OnPropertyChanged(nameof(IsReadyForVivoPatch));
        OnPropertyChanged(nameof(IsReadyForOfficialKsu));
        OnPropertyChanged(nameof(IsReadyForPatch));
        OnPropertyChanged(nameof(IsReadyForAutomaticRoot));
        OnPropertyChanged(nameof(PreflightSummary));
        PatchImageCommand.NotifyCanExecuteChanged();
        RunAutomaticRootCommand.NotifyCanExecuteChanged();
    }

    private void NotifyKmiChanged()
    {
        OnPropertyChanged(nameof(EffectiveKmi));
        OnPropertyChanged(nameof(DetectedKmi));
        ResolveKmiCommand.NotifyCanExecuteChanged();
        NotifyPreflightChanged();
    }

    private bool CanResolveKmi() => backend is not null && session.IsAdbConnected && !session.IsBusy;

    private bool CanRunAutomaticRoot() => IsReadyForAutomaticRoot && backend is not null && !session.IsBusy;

    private bool IsInitBootImage => IsSupportedRootImage(SelectedImage);

    private bool IsVendorBootImage => IsSupportedRootImage(SelectedVendorImage);

    private static bool IsSupportedRootImage(FlashImageInfo? image)
    {
        if (image is null)
        {
            return false;
        }

        var extension = Path.GetExtension(image.Path);
        return string.Equals(extension, ".img", StringComparison.OrdinalIgnoreCase) ||
               string.Equals(extension, ".bin", StringComparison.OrdinalIgnoreCase);
    }

    private static string FormatBytes(long sizeBytes) => sizeBytes switch
    {
        < 1024 => $"{sizeBytes} B",
        < 1024 * 1024 => $"{sizeBytes / 1024d:F1} KB",
        < 1024L * 1024 * 1024 => $"{sizeBytes / 1024d / 1024:F1} MB",
        _ => $"{sizeBytes / 1024d / 1024 / 1024:F2} GB"
    };
}
