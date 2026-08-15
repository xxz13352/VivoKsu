using System.IO;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

/// <summary>
/// 「软件」页:展示本工具与其依赖组件的就绪状态——VivoKsu 版本、手机 USB 驱动、
/// scrcpy 投屏工具、payload_dumper 固件解包工具。检测在线程池执行(DriverStore 枚举可能很慢),
/// 完成后经 ObservableProperty 通知界面,避免 UI 卡顿。
/// scrcpy 状态感知内置目录 / PATH / 用户自选路径 / 自动下载安装目录四类来源。
/// 默认构造用真实组件检测(<see cref="AppContext.BaseDirectory"/> 下的发布目录);测试可注入 stub。
/// </summary>
public partial class SoftwareViewModel : ObservableObject
{
    private readonly VivoDriverDetector driverDetector;
    private readonly IScrcpyToolLocator scrcpyLocator;
    private readonly PayloadDumperRunner payloadDumper;
    private readonly PayloadDumperProvisioner? provisioner;
    private readonly ToolPathPreferences? preferences;
    private readonly string scrcpyInstallationRoot;
    private readonly Action? onReinstallDriver;

    public SoftwareViewModel() : this(AppContext.BaseDirectory)
    {
    }

    public SoftwareViewModel(
        string applicationRoot,
        VivoDriverDetector? driverDetector = null,
        IScrcpyToolLocator? scrcpyLocator = null,
        PayloadDumperRunner? payloadDumper = null,
        ToolPathPreferences? preferences = null,
        string? scrcpyInstallationRoot = null,
        Action? onReinstallDriver = null,
        PayloadDumperProvisioner? provisioner = null)
    {
        AppVersion = AppInfo.Version;
        this.driverDetector = driverDetector ?? VivoDriverDetector.CreateDefault();
        this.scrcpyLocator = scrcpyLocator ?? new ScrcpyToolLocator(applicationRoot);
        this.provisioner = provisioner;
        // 发布版不随包:runner 指向 provisioner 缓存路径(未下载则 IsAvailable=false)。
        this.payloadDumper = payloadDumper ?? new PayloadDumperRunner(
            provisioner?.ExecutablePath ?? Path.Combine(applicationRoot, "payload-tools", "payload_dumper.exe"));
        this.preferences = preferences;
        this.scrcpyInstallationRoot = scrcpyInstallationRoot ?? Path.Combine(ExternalResourceLocations.Root, "scrcpy");
        this.onReinstallDriver = onReinstallDriver;
        RefreshCommand = new AsyncRelayCommand(() => RefreshAsync());
        ReinstallDriverCommand = new RelayCommand(ReinstallDriver, () => onReinstallDriver is not null);

        // 页面未打开前即开始检测;完成后自动更新绑定。
        _ = RefreshAsync();
    }

    /// <summary>VivoKsu 客户端版本号(来自 csproj 的 &lt;Version&gt;)。</summary>
    public string AppVersion { get; }

    [ObservableProperty]
    private bool isAdbDriverInstalled;

    [ObservableProperty]
    private string adbDriverStatusText = "检测中…";

    [ObservableProperty]
    private bool isFastbootDriverInstalled;

    [ObservableProperty]
    private string fastbootDriverStatusText = "检测中…";

    [ObservableProperty]
    private bool isMediaTekDriverInstalled;

    [ObservableProperty]
    private string mediaTekDriverStatusText = "检测中…";

    [ObservableProperty]
    private bool isScrcpyReady;

    [ObservableProperty]
    private string scrcpyStatusText = "检测中…";

    [ObservableProperty]
    private bool isPayloadReady;

    [ObservableProperty]
    private string payloadStatusText = "检测中…";

    /// <summary>重新检测各组件状态(线程池执行,完成通知界面)。</summary>
    public IAsyncRelayCommand RefreshCommand { get; }

    /// <summary>重新安装 USB 驱动(弹驱动安装窗,三类可重装)。</summary>
    public IRelayCommand ReinstallDriverCommand { get; }

    private void ReinstallDriver()
    {
        onReinstallDriver?.Invoke();
        // 安装窗关闭后刷新,让软件页驱动状态与真实情况一致。
        _ = RefreshAsync();
    }

    private async Task RefreshAsync()
    {
        try
        {
            // DriverStore 枚举可能遍历上千目录,放线程池,避免冻结 UI。
            var driverTask = Task.Run(() =>
            {
                var adb = driverDetector.IsAdbInstalled;
                var fastboot = driverDetector.IsFastbootInstalled;
                var mediaTek = driverDetector.IsMediaTekInstalled;
                return (Adb: adb, Fastboot: fastboot, MediaTek: mediaTek);
            });
            var payloadReady = await Task.Run(() => payloadDumper.IsAvailable).ConfigureAwait(false);

            var scrcpyExecutable = await ResolveScrcpyAsync().ConfigureAwait(false);
            var (adb, fastboot, mediaTek) = await driverTask.ConfigureAwait(false);

            IsAdbDriverInstalled = adb;
            AdbDriverStatusText = adb ? "已安装" : "未安装";
            IsFastbootDriverInstalled = fastboot;
            FastbootDriverStatusText = fastboot ? "已安装" : "未安装";
            IsMediaTekDriverInstalled = mediaTek;
            MediaTekDriverStatusText = mediaTek ? "已安装" : "未安装";
            IsScrcpyReady = scrcpyExecutable is not null;
            ScrcpyStatusText = scrcpyExecutable is not null ? "scrcpy 已就绪" : "未检测到 scrcpy.exe";
            IsPayloadReady = payloadReady;
            PayloadStatusText = payloadReady
                ? "就绪"
                : provisioner is not null ? "未随包分发,首次固件提取时自动下载" : "未就绪";
        }
        catch
        {
            // 检测失败(DriverStore 权限等)不弹错误,把仍处于「检测中」的状态标为失败。
            if (AdbDriverStatusText == "检测中…") AdbDriverStatusText = "检测失败";
            if (FastbootDriverStatusText == "检测中…") FastbootDriverStatusText = "检测失败";
            if (MediaTekDriverStatusText == "检测中…") MediaTekDriverStatusText = "检测失败";
            if (ScrcpyStatusText == "检测中…") ScrcpyStatusText = "检测失败";
            if (PayloadStatusText == "检测中…") PayloadStatusText = "检测失败";
        }
    }

    /// <summary>解析 scrcpy 实际可用路径:内置/PATH(定位器)→ 用户自选 → 自动下载安装目录。</summary>
    private async Task<string?> ResolveScrcpyAsync()
    {
        // 定位器查内置目录与 PATH;若已就绪直接用。
        var viaLocator = await Task.Run(() => scrcpyLocator.IsAvailable ? scrcpyLocator.ExecutablePath : null).ConfigureAwait(false);
        if (viaLocator is not null)
        {
            return viaLocator;
        }

        // 用户自选路径(ADB 投屏页选择并持久化)。
        var selected = preferences?.ScrcpyPath;
        if (!string.IsNullOrWhiteSpace(selected) && File.Exists(selected))
        {
            return selected;
        }

        // 自动下载到 %LocalAppData%\VivoKsu\scrcpy 的安装目录。
        if (Directory.Exists(scrcpyInstallationRoot))
        {
            var provisioned = await Task.Run(() =>
                Directory.EnumerateFiles(scrcpyInstallationRoot, "scrcpy.exe", SearchOption.AllDirectories)
                    .FirstOrDefault(p => !Path.GetFileName(Path.GetDirectoryName(p))!.StartsWith(".staging-", StringComparison.OrdinalIgnoreCase)))
                .ConfigureAwait(false);
            if (provisioned is not null)
            {
                return provisioned;
            }
        }

        return null;
    }
}
