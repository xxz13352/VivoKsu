using System.Diagnostics;
using System.IO;
using Microsoft.Win32;

namespace VivoKsu.App.Services;

/// <summary>
/// .NET Desktop Runtime(WindowsDesktop.App)检测。framework-dependent 发布后 App 本体无法
/// 自检运行时(App 需要运行时才能启动),必须由入口引导器检测:已装则直接拉起 App;缺失则
/// 下载微软直链并静默安装。本类同时编入 <c>VivoKsu.Bootstrapper</c>(原生 AOT 引导器)
/// 与本程序集(便于测试),检测逻辑必须保持 AOT 兼容(无反射、无 WPF 依赖)。
/// </summary>
public static class DotNetRuntimeDetector
{
    /// <summary>默认的 dotnet 主机路径(桌面运行时随它一起安装)。</summary>
    internal const string DotNetExecutablePath = @"C:\Program Files\dotnet\dotnet.exe";

    private const string WindowsDesktopRuntimeLinePrefix = "Microsoft.WindowsDesktop.App ";
    private const string RegistrySharedFxSubKey =
        @"SOFTWARE\dotnet\Setup\InstalledVersions\x64\sharedfx\Microsoft.WindowsDesktop.App";

    /// <summary>
    /// 是否已安装可运行 net8.0 App 的 .NET Desktop Runtime(即 WindowsDesktop.App 8.x)。
    /// 依次探测:HKLM/HKCU 注册表 → <c>dotnet --list-runtimes</c> 兜底。
    /// 只认 major==8:App 未开 RollForward,9/10 运行时无法承载 net8.0 目标。
    /// </summary>
    public static bool HasDesktopRuntime8() => HasDesktopRuntime8(RegistryProbe, ProcessProbe);

    /// <summary>可注入探测器的重载(测试用)。registry 返回 null 表示“注册表没给出结论”,回退 process。</summary>
    internal static bool HasDesktopRuntime8(Func<bool?> registryProbe, Func<bool> processProbe)
    {
        var registryResult = registryProbe();
        return registryResult ?? processProbe();
    }

    /// <summary>注册表探测:返回 null 表示注册表不可用/无结论(调用方应回退进程探测)。</summary>
    private static bool? RegistryProbe()
    {
        foreach (var hive in new[] { RegistryHive.LocalMachine, RegistryHive.CurrentUser })
        {
            try
            {
                using var key = RegistryKey.OpenBaseKey(hive, RegistryView.Registry64)
                    .OpenSubKey(RegistrySharedFxSubKey);
                if (key is null)
                {
                    continue;
                }

                if (IsDesktop8OrNewer(key.GetValue("Version") as string))
                {
                    return true;
                }
            }
            catch
            {
                // 无权限/注册表被重定向等;交给下一个 hive 或进程探测。
            }
        }

        return null;
    }

    /// <summary>进程探测:运行 <c>dotnet --list-runtimes</c> 找 WindowsDesktop.App 8.x。</summary>
    private static bool ProcessProbe()
    {
        try
        {
            if (!File.Exists(DotNetExecutablePath))
            {
                return false;
            }

            var startInfo = new ProcessStartInfo(DotNetExecutablePath, "--list-runtimes")
            {
                UseShellExecute = false,
                RedirectStandardOutput = true,
                CreateNoWindow = true
            };
            using var process = Process.Start(startInfo);
            if (process is null)
            {
                return false;
            }

            var output = process.StandardOutput.ReadToEnd();
            process.WaitForExit(10_000);
            return output.Split('\n')
                .Where(line => line.StartsWith(WindowsDesktopRuntimeLinePrefix, StringComparison.Ordinal))
                .Select(ParseVersionFromListRuntimesLine)
                .Any(IsDesktop8OrNewer);
        }
        catch
        {
            return false;
        }
    }

    /// <summary>从 <c>dotnet --list-runtimes</c> 的一行(<c>Microsoft.WindowsDesktop.App 8.0.23 [路径]</c>)提取版本号。</summary>
    internal static string? ParseVersionFromListRuntimesLine(string line)
    {
        if (!line.StartsWith(WindowsDesktopRuntimeLinePrefix, StringComparison.Ordinal))
        {
            return null;
        }

        var rest = line[WindowsDesktopRuntimeLinePrefix.Length..].TrimStart();
        var end = rest.IndexOfAny([' ', '\t']);
        return end < 0 ? rest : rest[..end];
    }

    /// <summary>版本号形如 <c>8.0.30</c>;只认 major==8(App 未开 RollForward)。</summary>
    internal static bool IsDesktop8OrNewer(string? version)
    {
        if (string.IsNullOrWhiteSpace(version))
        {
            return false;
        }

        var major = version.Split('.')[0];
        return int.TryParse(major, out var majorNumber) && majorNumber == 8;
    }

    /// <summary>兼容旧式注册表布局(<c>…\InstalledVersions\x64\Microsoft.WindowsDesktop.App</c>),与
    /// <see cref="RegistrySharedFxSubKey"/> 二选一探测;保留以供未来布局迁移。</summary>
    internal static string? ReadLegacyRegistryVersion()
    {
        try
        {
            using var key = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, RegistryView.Registry64)
                .OpenSubKey(@"SOFTWARE\dotnet\Setup\InstalledVersions\x64\Microsoft.WindowsDesktop.App");
            return key?.GetValue("Version") as string;
        }
        catch
        {
            return null;
        }
    }
}
