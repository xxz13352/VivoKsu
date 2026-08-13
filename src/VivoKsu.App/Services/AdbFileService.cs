using System.IO;
using System.Text.RegularExpressions;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class AdbFileService
{
    private static readonly Regex LsLine = new(
        @"^(?<type>[bcdlps-])\S{9}\s+\d+\s+\S+\s+\S+\s+(?<size>\d+)\s+\S+\s+\S+\s+(?<name>.+)$",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    private readonly FastbootRsBackend backend;
    private readonly OperationLogService logs;

    public AdbFileService(FastbootRsBackend backend, OperationLogService logs)
    {
        this.backend = backend;
        this.logs = logs;
    }

    public async Task<IReadOnlyList<DeviceFileEntry>> ListRemoteAsync(
        string serial,
        string path,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        var normalizedPath = NormalizeRemotePath(path);
        var listingPath = normalizedPath == "/" ? "/" : $"{normalizedPath}/";
        context?.ReportStage($"正在读取设备目录 {normalizedPath}");
        var output = await backend.ShellAsync(serial, $"ls -laL -- {QuoteRemotePath(listingPath)}", cancellationToken);
        return output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Select(line => ParseEntry(normalizedPath, line))
            .Where(entry => entry is not null)
            .Cast<DeviceFileEntry>()
            .OrderByDescending(entry => entry.IsDirectory)
            .ThenBy(entry => entry.Name, StringComparer.OrdinalIgnoreCase)
            .ToArray();
    }

    public async Task UploadAsync(
        string serial,
        string localPath,
        string remoteDirectory,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        var destination = CombineRemotePath(remoteDirectory, Path.GetFileName(localPath));
        Report(context, OperationLogLevel.Info, $"正在上传 {Path.GetFileName(localPath)} 到 {destination}。");
        await backend.PushAsync(serial, localPath, destination, cancellationToken);
        Report(context, OperationLogLevel.Success, "文件上传完成。");
    }

    public async Task DownloadAsync(
        string serial,
        DeviceFileEntry remoteFile,
        string localDirectory,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        var destination = BuildSafeLocalDestination(localDirectory, remoteFile.Name);
        Report(context, OperationLogLevel.Info, $"正在下载 {remoteFile.FullPath}。");
        await backend.PullAsync(serial, remoteFile.FullPath, destination, cancellationToken);
        Report(context, OperationLogLevel.Success, "文件下载完成。");
    }

    public async Task DeleteRemoteAsync(
        string serial,
        DeviceFileEntry entry,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        Report(context, OperationLogLevel.Warning, $"正在删除设备文件: {entry.FullPath}。");
        await backend.ShellAsync(serial, $"rm -rf -- {QuoteRemotePath(entry.FullPath)}", cancellationToken);
        Report(context, OperationLogLevel.Success, "设备文件已删除。");
    }

    public async Task<string> InstallApkAsync(
        string serial,
        string apkPath,
        CancellationToken cancellationToken,
        OperationContext? context = null)
    {
        if (!string.Equals(Path.GetExtension(apkPath), ".apk", StringComparison.OrdinalIgnoreCase))
        {
            throw new ArgumentException("仅支持安装 .apk 文件。", nameof(apkPath));
        }

        Report(context, OperationLogLevel.Info, $"正在安装 APK: {Path.GetFileName(apkPath)}。");
        var result = await backend.InstallAsync(serial, apkPath, true, cancellationToken);
        Report(context, OperationLogLevel.Success, "APK 安装指令已完成。");
        return result;
    }

    private void Report(OperationContext? context, OperationLogLevel level, string message)
    {
        if (context is null)
        {
            logs.Write(level, message);
            return;
        }

        context.ReportStage(message.TrimEnd('。'));
    }

    private static DeviceFileEntry? ParseEntry(string directory, string line)
    {
        var match = LsLine.Match(line);
        if (!match.Success)
        {
            return null;
        }

        var name = match.Groups["name"].Value;
        var linkTargetSeparator = name.IndexOf(" -> ", StringComparison.Ordinal);
        if (linkTargetSeparator >= 0)
        {
            name = name[..linkTargetSeparator];
        }

        if (name is "." or "..")
        {
            return null;
        }

        return new DeviceFileEntry(
            name,
            CombineRemotePath(directory, name),
            match.Groups["type"].Value == "d",
            long.Parse(match.Groups["size"].Value));
    }

    private static string NormalizeRemotePath(string path)
    {
        if (string.IsNullOrWhiteSpace(path))
        {
            return "/sdcard";
        }

        var normalized = path.Trim();
        return normalized == "/" ? "/" : normalized.TrimEnd('/');
    }

    private static string QuoteRemotePath(string path) =>
        $"'{path.Replace("'", "'\\''", StringComparison.Ordinal)}'";

    private static string CombineRemotePath(string directory, string name)
    {
        var normalizedDirectory = NormalizeRemotePath(directory);
        return normalizedDirectory == "/" ? $"/{name}" : $"{normalizedDirectory}/{name}";
    }

    private static string BuildSafeLocalDestination(string localDirectory, string fileName)
    {
        if (string.IsNullOrWhiteSpace(fileName)
            || fileName is "." or ".."
            || fileName.EndsWith(' ')
            || fileName.EndsWith('.')
            || Path.IsPathRooted(fileName)
            || fileName.Any(character => character < ' ' || Path.GetInvalidFileNameChars().Contains(character))
            || IsReservedWindowsFileName(fileName))
        {
            throw new ArgumentException("设备文件名无法安全保存到 Windows。", nameof(fileName));
        }

        var normalizedDirectory = Path.GetFullPath(localDirectory);
        var destination = Path.GetFullPath(Path.Combine(normalizedDirectory, fileName));
        var directoryPrefix = Path.EndsInDirectorySeparator(normalizedDirectory)
            ? normalizedDirectory
            : normalizedDirectory + Path.DirectorySeparatorChar;
        if (!destination.StartsWith(directoryPrefix, StringComparison.OrdinalIgnoreCase))
        {
            throw new ArgumentException("下载目标超出当前本地目录。", nameof(fileName));
        }

        return destination;
    }

    private static bool IsReservedWindowsFileName(string fileName)
    {
        var stem = fileName.Split('.')[0];
        if (stem.Equals("CON", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("PRN", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("AUX", StringComparison.OrdinalIgnoreCase)
            || stem.Equals("NUL", StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }

        return stem.Length == 4
            && (stem.StartsWith("COM", StringComparison.OrdinalIgnoreCase)
                || stem.StartsWith("LPT", StringComparison.OrdinalIgnoreCase))
            && stem[3] is >= '1' and <= '9';
    }
}
