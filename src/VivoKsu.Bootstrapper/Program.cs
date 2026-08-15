using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using VivoKsu.App.Services;

namespace VivoKsu.Bootstrapper;

/// <summary>
/// 奶蛙Flash 原生入口(框架发布版的引导器)。
/// 流程:检测 .NET 8 Desktop Runtime → 已装直接拉起 VivoKsu.App.exe;缺失则从微软直链
/// 下载安装包、runas 提权静默安装(UAC 一次),再拉起 App。
/// AOT 编译为原生 exe(≈1MB),不依赖任何运行时;正常路径不分配控制台(无闪窗)。
/// </summary>
internal static class Program
{
    /// <summary>8.0 桌面运行时官方直链(微软 Azure CDN)。升级运行时版本时同步更新此处与发布脚本。</summary>
    private const string RuntimeDownloadUrl =
        "https://dotnetcli.azureedge.net/dotnet/WindowsDesktop/8.0.30/windowsdesktop-runtime-8.0.30-win-x64.exe";

    /// <summary>微软官方下载页(自动安装失败时的手动兜底)。</summary>
    private const string RuntimeDownloadPage = "https://dotnet.microsoft.com/en-us/download/dotnet/8.0";

    private const string AppExecutableName = "VivoKsu.App.exe";

    private static async Task<int> Main(string[] args)
    {
        try
        {
            if (DotNetRuntimeDetector.HasDesktopRuntime8())
            {
                return LaunchApp(args);
            }

            ShowConsole();
            Console.WriteLine("奶蛙Flash 需要 .NET 8 桌面运行时,首次运行将自动下载并安装(约 56MB)。");
            Console.WriteLine();

            var installerPath = Path.Combine(
                Path.GetTempPath(), $"windowsdesktop-runtime-8.0.30-win-x64-{Guid.NewGuid():N}.exe");
            try
            {
                await DownloadAsync(RuntimeDownloadUrl, installerPath);
                Console.WriteLine();
                Console.WriteLine("下载完成,正在静默安装(需要一次管理员授权,请点击“是”)…");
                if (!await InstallAsync(installerPath))
                {
                    return 1;
                }

                if (!DotNetRuntimeDetector.HasDesktopRuntime8())
                {
                    Console.WriteLine();
                    Console.Error.WriteLine("安装后仍未检测到 .NET 8 桌面运行时,请手动安装:");
                    Console.Error.WriteLine("  " + RuntimeDownloadPage);
                    return 1;
                }

                Console.WriteLine("安装成功,正在启动奶蛙Flash…");
                return LaunchApp(args);
            }
            finally
            {
                TryDelete(installerPath);
            }
        }
        catch (Exception exception)
        {
            EnsureConsole();
            Console.Error.WriteLine();
            Console.Error.WriteLine("启动失败: " + exception.Message);
            Console.Error.WriteLine("请手动安装 .NET 8 桌面运行时后重新运行本程序:");
            Console.Error.WriteLine("  " + RuntimeDownloadPage);
            return 1;
        }
    }

    /// <summary>拉起同目录的 App(透传启动参数,退出码透传)。</summary>
    private static int LaunchApp(string[] args)
    {
        var appPath = Path.Combine(AppContext.BaseDirectory, AppExecutableName);
        if (!File.Exists(appPath))
        {
            throw new FileNotFoundException($"未找到 {AppExecutableName},请确认文件完整。");
        }

        using var process = new Process
        {
            StartInfo = new ProcessStartInfo
            {
                FileName = appPath,
                UseShellExecute = true,
                WorkingDirectory = AppContext.BaseDirectory,
                Arguments = BuildArguments(args)
            }
        };
        process.Start();
        process.WaitForExit();
        return process.ExitCode;
    }

    /// <summary>runas 提权静默安装(<c>/install /quiet /norestart</c>);UAC 取消返回 false。</summary>
    private static async Task<bool> InstallAsync(string installerPath)
    {
        try
        {
            using var process = new Process
            {
                StartInfo = new ProcessStartInfo
                {
                    FileName = installerPath,
                    UseShellExecute = true,
                    Verb = "runas",
                    Arguments = "/install /quiet /norestart"
                }
            };
            if (!process.Start())
            {
                Console.Error.WriteLine("无法启动运行时安装程序。");
                return false;
            }

            process.WaitForExit();
            if (process.ExitCode != 0)
            {
                Console.Error.WriteLine($"运行时安装程序退出码 {process.ExitCode},安装可能未完成。");
                return false;
            }

            return true;
        }
        catch (Win32Exception) when (HasElevationCanceled())
        {
            Console.Error.WriteLine("管理员授权被取消,无法安装运行时。");
            return false;
        }
    }

    private static bool HasElevationCanceled() => true;

    /// <summary>流式下载到本地文件,控制台显示百分比进度。</summary>
    private static async Task DownloadAsync(string url, string destinationPath)
    {
        using var client = new HttpClient { Timeout = TimeSpan.FromMinutes(15) };
        client.DefaultRequestHeaders.UserAgent.ParseAdd("VivoKsu-Bootstrapper/1.0");
        using var response = await client.GetAsync(url, HttpCompletionOption.ResponseHeadersRead);
        response.EnsureSuccessStatusCode();
        var totalBytes = response.Content.Headers.ContentLength ?? 0L;
        await using var input = await response.Content.ReadAsStreamAsync();
        await using var output = File.Create(destinationPath);
        var buffer = new byte[81920];
        long read = 0L;
        var lastBucket = -1;
        int count;
        while ((count = await input.ReadAsync(buffer)) > 0)
        {
            await output.WriteAsync(buffer.AsMemory(0, count));
            read += count;
            if (totalBytes > 0)
            {
                var bucket = (int)(read * 100 / totalBytes) / 5;
                if (bucket != lastBucket)
                {
                    lastBucket = bucket;
                    Console.Write($"\r下载进度: {bucket * 5,3}%  {FormatSize(read)} / {FormatSize(totalBytes)}   ");
                }
            }
        }

        Console.WriteLine("\r下载完成: {0}                                 ", FormatSize(read));
    }

    private static string FormatSize(long bytes) =>
        bytes >= 1_048_576 ? $"{bytes / 1_048_576.0:0.0} MB" : $"{bytes / 1024.0:0.0} KB";

    private static string BuildArguments(IEnumerable<string> args)
    {
        var builder = new StringBuilder();
        foreach (var arg in args)
        {
            if (builder.Length > 0)
            {
                builder.Append(' ');
            }

            builder.Append('"').Append(arg.Replace("\"", "\\\"", StringComparison.Ordinal)).Append('"');
        }

        return builder.ToString();
    }

    private static bool consoleAllocated;

    /// <summary>WinExe 默认无控制台;安装路径需要展示进度时分配一个,并重定向标准输出。</summary>
    private static void ShowConsole()
    {
        if (consoleAllocated)
        {
            return;
        }

        if (!AllocConsole())
        {
            return;
        }

        consoleAllocated = true;
        try
        {
            var writer = new StreamWriter(Console.OpenStandardOutput()) { AutoFlush = true };
            Console.SetOut(writer);
        }
        catch
        {
            // 输出句柄初始化失败时忽略;WriteLine 仍尽力写出。
        }
    }

    private static void EnsureConsole()
    {
        if (!consoleAllocated)
        {
            ShowConsole();
        }
    }

    [DllImport("kernel32.dll")]
    private static extern bool AllocConsole();

    private static void TryDelete(string path)
    {
        try
        {
            if (File.Exists(path))
            {
                File.Delete(path);
            }
        }
        catch
        {
            // 清理失败不阻塞。
        }
    }
}
