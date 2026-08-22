using System.IO;
using Microsoft.Win32;

namespace VivoKsu.App.Services;

/// <summary>
/// vivo USB 驱动安装检测,分三类单独判定:
/// <list type="bullet">
///   <item><b>ADB</b>(WinUSB,`android_winusb`):连接 / 文件管理 / 投屏所需;</item>
///   <item><b>Fastboot</b>(`android_usb`):fastbootd 刷写所需;</item>
///   <item><b>MediaTek 联发科</b>(`cdc-acm` + FTDI):预加载 / BROM 救砖串口驱动。</item>
/// </list>
/// 每类命中任一信号即视为已具备:
/// 1) DriverStore 里已注册该类 INF(含 Windows 通用 ADB/串口驱动——它们提供相同能力);
/// 2) 旧版全量驱动包安装目录存在(默认 C:\Program Files\BBK\vivo_usb_driver,视为三类齐备);
/// 3) 旧版全量驱动包卸载注册表项存在(键名 vivo_usb_driver_is1)。
/// 路径与注册表查询均可经构造函数注入,便于单元测试。
/// </summary>
public sealed class VivoDriverDetector
{
    // 本驱动包内置各分类的 INF 基名(不含扩展名)。驱动包 ADB 用的是 android_winusb(带下划线,
    // Google 同款,提供相同 ADB 能力);MediaTek 分类只用 cdc-acm 且须 INF 内容含 "MediaTek"——
    // 排除 CH340/FTDI/其它 CDC 串口设备的假阳性(这些不提供联发科 VCOM 能力)。
    private static readonly string[] AdbMarkers = ["android_winusb"];
    private static readonly string[] FastbootMarkers = ["android_usb"];

    private readonly IReadOnlyList<string> driverStoreDirectories;
    private readonly IReadOnlyList<string> installDirectories;
    private readonly IReadOnlyList<string> uninstallRegistryKeys;
    private readonly Func<string, bool> registryKeyExists;

    /// <summary>
    /// 默认取本机真实路径:驱动存储目录、BBK 安装目录、卸载注册表键。
    /// 测试可注入假路径/假注册表查询。
    /// </summary>
    public VivoDriverDetector(
        IReadOnlyList<string>? driverStoreDirectories = null,
        IReadOnlyList<string>? installDirectories = null,
        IReadOnlyList<string>? uninstallRegistryKeys = null,
        Func<string, bool>? registryKeyExists = null)
    {
        this.driverStoreDirectories = driverStoreDirectories ?? [DefaultDriverStoreDirectory()];
        this.installDirectories = installDirectories ?? DefaultInstallDirectories();
        this.uninstallRegistryKeys = uninstallRegistryKeys ?? DefaultUninstallRegistryKeys();
        this.registryKeyExists = registryKeyExists ?? RegistryKeyExists;
    }

    /// <summary>默认检测器:读取本机真实环境。</summary>
    public static VivoDriverDetector CreateDefault() => new();

    /// <summary>ADB(WinUSB)驱动是否已安装。</summary>
    public bool IsAdbInstalled => HasAnyDriverStoreMarker(AdbMarkers) || HasLegacyInstall();

    /// <summary>Fastboot 驱动是否已安装。</summary>
    public bool IsFastbootInstalled => HasAnyDriverStoreMarker(FastbootMarkers) || HasLegacyInstall();

    /// <summary>MediaTek 联发科串口驱动是否已安装。</summary>
    public bool IsMediaTekInstalled => HasMediaTekDriver() || HasLegacyInstall();

    /// <summary>三类驱动是否全部安装。</summary>
    public bool IsAllInstalled => IsAdbInstalled && IsFastbootInstalled && IsMediaTekInstalled;

    /// <summary>是否至少有一类已安装。</summary>
    public bool IsAnyInstalled => IsAdbInstalled || IsFastbootInstalled || IsMediaTekInstalled;

    /// <summary>兼容旧调用:任一驱动存在即视为「已安装」(启动提醒按「是否全部齐备」判断,见 <see cref="IsAllInstalled"/>)。</summary>
    public bool IsInstalled() => IsAnyInstalled;

    private bool HasAnyDriverStoreMarker(IReadOnlyList<string> markers)
    {
        var markerSet = new HashSet<string>(markers, StringComparer.OrdinalIgnoreCase);
        foreach (var directory in driverStoreDirectories)
        {
            if (!Directory.Exists(directory))
            {
                continue;
            }

            foreach (var folder in Directory.EnumerateDirectories(directory))
            {
                var infName = Path.GetFileName(folder);
                var dotIndex = infName.IndexOf('.');
                if (dotIndex > 0 && markerSet.Contains(infName[..dotIndex]))
                {
                    return true;
                }
            }
        }

        return false;
    }

    /// <summary>
    /// MediaTek 判定:DriverStore 里 cdc-acm 目录的 INF 内容须含 "MediaTek"。
    /// cdc-acm 是通用 USB 串口类名(CH340/FTDI 等其它厂商也会 staging),仅凭目录名会假阳性;
    /// 联发科 VCOM 驱动(本驱动包 cdc-acm.inf)的 Provider 正是 "MediaTek Inc."。
    /// </summary>
    private bool HasMediaTekDriver()
    {
        foreach (var directory in driverStoreDirectories)
        {
            if (!Directory.Exists(directory))
            {
                continue;
            }

            foreach (var folder in Directory.EnumerateDirectories(directory))
            {
                var folderName = Path.GetFileName(folder);
                if (!folderName.StartsWith("cdc-acm", StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }

                try
                {
                    var inf = Directory.EnumerateFiles(folder, "*.inf", SearchOption.TopDirectoryOnly).FirstOrDefault();
                    if (inf is not null && File.ReadAllText(inf).Contains("MediaTek", StringComparison.OrdinalIgnoreCase))
                    {
                        return true;
                    }
                }
                catch
                {
                    // 目录被并发清理/权限异常:按不存在处理,继续扫描。
                }
            }
        }

        return false;
    }

    /// <summary>
    /// 旧版全量驱动包信号:安装目录存在且内含 INF(空残留目录不算),或卸载注册表项存在。
    /// 卸载残留的空目录若算齐备,会在驱动实际已删时误报「已安装」。
    /// </summary>
    private bool HasLegacyInstall() =>
        installDirectories.Any(dir =>
            Directory.Exists(dir)
            && Directory.EnumerateFiles(dir, "*.inf", SearchOption.AllDirectories).Any())
        || uninstallRegistryKeys.Any(registryKeyExists);

    private static string DefaultDriverStoreDirectory() =>
        Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.Windows),
            "System32",
            "DriverStore",
            "FileRepository");

    private static IReadOnlyList<string> DefaultInstallDirectories()
    {
        var directories = new List<string>();
        var programFiles = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles);
        var programFilesX86 = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86);
        foreach (var root in new[] { programFiles, programFilesX86 }.Distinct(StringComparer.OrdinalIgnoreCase))
        {
            if (!string.IsNullOrWhiteSpace(root))
            {
                directories.Add(Path.Combine(root, "BBK", "vivo_usb_driver"));
            }
        }

        return directories;
    }

    private static IReadOnlyList<string> DefaultUninstallRegistryKeys() =>
    [
        @"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\vivo_usb_driver_is1",
        @"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\vivo_usb_driver_is1",
    ];

    private static bool RegistryKeyExists(string keyPath)
    {
        try
        {
            // 形如 "HKLM\SOFTWARE\...\Uninstall\xxx_is1";其余 hive 前缀暂不支持。
            if (!keyPath.StartsWith(@"HKLM\", StringComparison.OrdinalIgnoreCase))
            {
                return false;
            }

            var subKey = keyPath[@"HKLM\".Length..];
            return Registry.LocalMachine.OpenSubKey(subKey) is not null;
        }
        catch
        {
            // 权限/被删等异常一律按「不存在」处理,不让检测本身报错。
            return false;
        }
    }
}
