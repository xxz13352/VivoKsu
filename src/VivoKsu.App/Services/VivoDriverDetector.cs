using System.IO;
using Microsoft.Win32;

namespace VivoKsu.App.Services;

/// <summary>
/// vivo USB 驱动安装检测。任一信号命中即视为「已安装」:
/// 1) Windows 驱动存储 DriverStore 里已注册本驱动包的关键 INF(ADB WinUSB / fastboot / 联发科);
/// 2) 本驱动包的安装目录存在(兼容旧版全量驱动包,默认 C:\Program Files\BBK\vivo_usb_driver);
/// 3) 本驱动包在「卸载程序」中的注册表项存在(兼容旧版,键名 vivo_usb_driver_is1)。
/// 路径与注册表查询均可经构造函数注入,便于单元测试。
/// </summary>
public sealed class VivoDriverDetector
{
    // 本驱动包内置的关键 INF 文件名(不含扩展名)。任一出现在 DriverStore 即视为驱动能力已具备。
    // 注意:只用 vivo 驱动包自身安装的 INF 基名(androidwinusb=ADB、android_usb=fastboot),
    // 不用 cdc-acm/ftdibus/ftdiport 等通用串口名——它们会被任何 FTDI/CDC 设备 staging,造成假阳性。
    private static readonly string[] DriverStoreMarkers =
    [
        "androidwinusb",  // 本驱动包 ADB 驱动(vivo 特有命名;Google 的是 android_winusb,带下划线,不匹配)
        "android_usb",    // 本驱动包 fastboot 驱动
    ];

    private static readonly HashSet<string> MarkerSet =
        new(DriverStoreMarkers, StringComparer.OrdinalIgnoreCase);

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

    /// <summary>驱动是否已安装(存在任一信号即返回 true)。</summary>
    public bool IsInstalled() =>
        HasAnyDriverStoreMarker()
        || installDirectories.Any(Directory.Exists)
        || uninstallRegistryKeys.Any(registryKeyExists);

    private bool HasAnyDriverStoreMarker()
    {
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
                if (dotIndex <= 0)
                {
                    continue;
                }

                if (MarkerSet.Contains(infName[..dotIndex]))
                {
                    return true;
                }
            }
        }

        return false;
    }

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
