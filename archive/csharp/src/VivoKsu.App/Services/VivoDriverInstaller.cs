using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using SharpCompress.Archives;
using SharpCompress.Archives.SevenZip;
using SharpCompress.Readers;

namespace VivoKsu.App.Services;

/// <summary>
/// vivo USB 驱动安装器。随应用分发 <c>drivers\vivo-usb-driver.7z</c>(只含 ADB / fastboot / 联发科三类的解包驱动,
/// LZMA2 压缩)。安装流程:解压到临时目录 → 用系统 pnputil 提权逐 INF 安装 → 把 vivo VID 写入 adb_usb.ini → 清理临时目录。
/// 不再运行任何第三方安装器。
/// </summary>
public sealed class VivoDriverInstaller
{
    public const string ArchiveFileName = "vivo-usb-driver.7z";

    // 本驱动包需写入 adb_usb.ini 的 vivo VID(原 add_vids.bat 的行为)。
    private static readonly string[] VivoAdbVids = ["0x2D95", "0x9BB5", "0x18D1", "0x0E8D"];

    private readonly Func<ProcessStartInfo, Task<int>> startAndWait;
    private readonly Func<string, string, Task> archiveExtractor;
    private readonly string adbUsbIniPath;

    /// <summary>提权启动、7z 解压、adb_usb.ini 路径均可注入,便于单元测试。</summary>
    public VivoDriverInstaller(
        Func<ProcessStartInfo, Task<int>>? startAndWait = null,
        Func<string, string, Task>? archiveExtractor = null,
        string? adbUsbIniPath = null)
    {
        this.startAndWait = startAndWait ?? StartElevatedAndWaitAsync;
        this.archiveExtractor = archiveExtractor ?? ExtractArchiveAsync;
        this.adbUsbIniPath = adbUsbIniPath ?? DefaultAdbUsbIniPath();
    }

    /// <summary>定位随应用分发的驱动包;缺失返回 null(发布目录被裁剪等)。</summary>
    public static string? LocateBundle(string baseDirectory)
    {
        var path = Path.Combine(baseDirectory, "drivers", ArchiveFileName);
        return File.Exists(path) ? path : null;
    }

    /// <summary>
    /// 安装驱动。返回 pnputil 退出码(0 为成功);用户取消 UAC 授权时抛 <see cref="OperationCanceledException"/>。
    /// 安装成功后把 vivo VID 写入 adb_usb.ini(供 adb 识别设备)。
    /// </summary>
    public async Task<int> InstallAsync(string bundlePath, CancellationToken cancellationToken = default)
    {
        var staging = Path.Combine(Path.GetTempPath(), "VivoKsu", "drivers", Guid.NewGuid().ToString("N"));
        try
        {
            Directory.CreateDirectory(staging);
            await archiveExtractor(bundlePath, staging);

            // 解压前校验 staging 里有 INF;实际安装用通配符 + /subdirs 一次递归装全部。
            var infs = Directory.EnumerateFiles(staging, "*.inf", SearchOption.AllDirectories)
                .OrderBy(path => path, StringComparer.OrdinalIgnoreCase)
                .ToArray();
            if (infs.Length == 0)
            {
                throw new InvalidOperationException("驱动包内未找到任何 INF,请重新下载安装包。");
            }

            var startInfo = new ProcessStartInfo
            {
                FileName = Path.Combine(Environment.SystemDirectory, "pnputil.exe"),
                Arguments = BuildPnputilArguments(staging),
                UseShellExecute = true,
                Verb = "runas",
                CreateNoWindow = true,
            };

            var exitCode = await startAndWait(startInfo);
            cancellationToken.ThrowIfCancellationRequested();
            if (exitCode == 0)
            {
                WriteAdbUsbIni();
            }

            return exitCode;
        }
        finally
        {
            DeleteQuietly(staging);
        }
    }

    /// <summary>
    /// pnputil 一条命令递归安装 staging 下所有 INF。真实语法为
    /// <c>/add-driver &lt;filename.inf | *.inf&gt; [/subdirs] [/install]</c>——
    /// 只接受单个 INF 或通配符(不含 /quiet),多个显式 INF 路径会被整行拒绝。
    /// </summary>
    internal static string BuildPnputilArguments(string stagingDirectory)
        => $"/add-driver \"{stagingDirectory}\\*.inf\" /subdirs /install";

    private static string DefaultAdbUsbIniPath() =>
        Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            ".android",
            "adb_usb.ini");

    /// <summary>以管理员权限启动 pnputil 并等待结束;UAC 被拒(Win32 1223)转为取消异常。</summary>
    private static async Task<int> StartElevatedAndWaitAsync(ProcessStartInfo startInfo)
    {
        try
        {
            using var process = Process.Start(startInfo)
                ?? throw new InvalidOperationException("无法启动驱动安装程序。");
            await process.WaitForExitAsync();
            return process.ExitCode;
        }
        catch (Win32Exception exception) when (exception.NativeErrorCode == 1223)
        {
            // ERROR_CANCELLED:用户在 UAC 弹窗点了「否」。
            throw new OperationCanceledException("已取消管理员授权,未安装驱动。", exception);
        }
    }

    private static async Task ExtractArchiveAsync(string archivePath, string destination)
    {
        var destinationFull = Path.GetFullPath(destination);
        // 0.50.x 起 SevenZipArchive.Open 更名 OpenArchive(显式强制 7z,驱动包恒为 .7z)。
        using var archive = SevenZipArchive.OpenArchive(archivePath, new ReaderOptions());
        foreach (var entry in archive.Entries.Where(e => !e.IsDirectory))
        {
            // 0.50.x 起 Key 可空;7z 文件条目必有 Key,理论上的无路径条目直接跳过(避免解引用空引用)。
            if (entry.Key is null)
            {
                continue;
            }

            // 防路径穿越:entry.Key 含 ../ 或绝对路径时,归一化后必须仍以 staging 为前缀,否则拒绝。
            var target = Path.GetFullPath(Path.Combine(destinationFull, entry.Key.Replace('/', Path.DirectorySeparatorChar)));
            if (!target.StartsWith(destinationFull + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidOperationException("驱动包包含非法路径条目,已中止安装。");
            }

            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            using var input = entry.OpenEntryStream();
            using var output = File.Create(target);
            await input.CopyToAsync(output);
        }
    }

    private void WriteAdbUsbIni()
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(adbUsbIniPath)!);
            var existing = File.Exists(adbUsbIniPath)
                ? File.ReadAllLines(adbUsbIniPath)
                : Array.Empty<string>();
            // 去重前 trim:既有行带尾随空白/行内注释时不至于重复追加同一 VID。
            var present = new HashSet<string>(
                existing
                    .Select(line => line.Trim())
                    .Where(line => line.StartsWith("0x", StringComparison.OrdinalIgnoreCase)),
                StringComparer.OrdinalIgnoreCase);
            var missing = VivoAdbVids.Where(vid => !present.Contains(vid)).ToArray();
            if (missing.Length > 0)
            {
                File.AppendAllLines(adbUsbIniPath, missing);
            }
        }
        catch
        {
            // adb_usb.ini 写入失败不影响驱动安装结果(现代 adb 内置这些 VID)。
        }
    }

    private static void DeleteQuietly(string path)
    {
        try
        {
            if (Directory.Exists(path))
            {
                Directory.Delete(path, recursive: true);
            }
        }
        catch
        {
            // Best effort;下次启动的清理会继续。
        }
    }
}
