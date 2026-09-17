using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.RegularExpressions;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class AdbRootTransferRunner : IAdbRootTransferRunner
{
    // Device paths come from the device's own `readlink -f` output and are
    // embedded into the dd command unquoted, so only accept metacharacter-free
    // absolute block-device paths.
    private static readonly Regex SafeDevicePathRegex = new(
        "^/dev/block/[A-Za-z0-9._/-]+$",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    private readonly string adbExecutable;
    private readonly IAdbBinaryRunner binaryRunner;

    public AdbRootTransferRunner(string adbExecutable, IAdbBinaryRunner binaryRunner)
    {
        this.adbExecutable = adbExecutable;
        this.binaryRunner = binaryRunner;
    }

    public Task<string> RunRootAsync(string serial, string command, CancellationToken cancellationToken) =>
        binaryRunner.RunTextAsync(adbExecutable, WithRootCommand(serial, "shell", command), cancellationToken);

    public Task CopyFromDeviceAsync(
        string serial,
        string devicePath,
        string localPath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        // adb exec-out does not forward a quoted `su -c` argument to the device
        // shell intact: the whole dd command arrives as a single word and dd
        // never runs, so backups were tiny sh error files. Pass the dd command
        // as separate argv tokens instead, and redirect dd's progress summary
        // away from stdout so the stream is exactly the partition bytes.
        return binaryRunner.CopyFromDeviceAsync(
            adbExecutable,
            BuildReadCommand(serial, devicePath),
            localPath,
            progress,
            cancellationToken);
    }

    public Task CopyToDeviceAsync(
        string serial,
        string localImagePath,
        string devicePath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken) =>
        binaryRunner.CopyToDeviceAsync(
            adbExecutable,
            WithRootCommand(serial, "shell", $"dd of={ShellQuote(devicePath)} bs=4M conv=fsync", "-T"),
            localImagePath,
            progress,
            cancellationToken);

    public Task EraseAsync(
        string serial,
        string devicePath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken) =>
        RunRootAsync(
            serial,
            $"blkdiscard {ShellQuote(devicePath)} || dd if=/dev/zero of={ShellQuote(devicePath)} bs=4M conv=fsync",
            cancellationToken);

    private static IReadOnlyList<string> BuildReadCommand(string serial, string devicePath)
    {
        if (string.IsNullOrWhiteSpace(devicePath) || !SafeDevicePathRegex.IsMatch(devicePath))
        {
            throw new InvalidOperationException("无效的分区设备路径，已阻止读取。");
        }

        return WithSerial(serial, "exec-out", "su", "-c", "dd", $"if={devicePath}", "bs=4M", "2>/dev/null");
    }

    private static IReadOnlyList<string> WithSerial(string serial, params string[] arguments)
    {
        var values = new List<string>(arguments.Length + 2) { "-s", serial };
        values.AddRange(arguments);
        return values;
    }

    private static IReadOnlyList<string> WithRootCommand(string serial, string adbService, string command, params string[] serviceOptions)
    {
        var arguments = new List<string> { adbService };
        arguments.AddRange(serviceOptions);
        arguments.AddRange(["su", "-c", ShellQuote(command)]);
        return WithSerial(serial, arguments.ToArray());
    }

    private static string ShellQuote(string value) => $"'{value.Replace("'", "'\"'\"'", StringComparison.Ordinal)}'";
}

public sealed class SystemAdbBinaryRunner : IAdbBinaryRunner
{
    private const int BufferSize = 1024 * 1024;

    public async Task<string> RunTextAsync(string executable, IReadOnlyList<string> arguments, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        using var process = Start(executable, arguments, redirectStandardInput: false, redirectStandardOutput: true);
        var outputTask = process.StandardOutput.ReadToEndAsync();
        var errorTask = process.StandardError.ReadToEndAsync();
        try
        {
            await process.WaitForExitAsync(cancellationToken);
        }
        catch (Exception)
        {
            // 取消时必须终止 adb 进程树:否则设备侧的破坏性命令(dd 清零 / blkdiscard)
            // 会在 UI 报告「已取消」后继续执行,adb.exe 也泄漏。兄弟方法均走 KillProcessTree。
            KillProcessTree(process);
            throw;
        }

        var output = await outputTask;
        var error = await errorTask;
        ThrowForFailure(process.ExitCode, error, output);
        return string.Join(Environment.NewLine, new[] { output, error }.Where(value => !string.IsNullOrWhiteSpace(value)));
    }

    public async Task CopyFromDeviceAsync(
        string executable,
        IReadOnlyList<string> arguments,
        string localPath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        using var process = Start(executable, arguments, redirectStandardInput: false, redirectStandardOutput: true);
        var errorTask = process.StandardError.ReadToEndAsync();
        try
        {
            await using var destination = new FileStream(localPath, FileMode.Create, FileAccess.Write, FileShare.None, BufferSize, useAsync: true);
            await CopyWithProgressAsync(process.StandardOutput.BaseStream, destination, progress, cancellationToken);
        }
        catch (Exception exception)
        {
            KillProcessTree(process);
            var error = await ReadErrorTextAsync(errorTask);
            if (exception is OperationCanceledException)
            {
                throw;
            }

            throw new IOException(
                $"从设备读取失败：{(string.IsNullOrWhiteSpace(error) ? exception.Message : error)}",
                exception);
        }

        try
        {
            // 读取结尾的等待同样必须可取消并终止 adb(传输完成后 adb 收尾)。
            await process.WaitForExitAsync(cancellationToken);
        }
        catch (OperationCanceledException)
        {
            KillProcessTree(process);
            throw;
        }

        ThrowForFailure(process.ExitCode, await errorTask, string.Empty);
    }

    public async Task CopyToDeviceAsync(
        string executable,
        IReadOnlyList<string> arguments,
        string localPath,
        IProgress<PartitionTransferProgress>? progress,
        CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        using var process = Start(executable, arguments, redirectStandardInput: true, redirectStandardOutput: false);
        var errorTask = process.StandardError.ReadToEndAsync();
        try
        {
            await using var source = new FileStream(localPath, FileMode.Open, FileAccess.Read, FileShare.Read, BufferSize, useAsync: true);
            await CopyWithProgressAsync(source, process.StandardInput.BaseStream, progress, cancellationToken);
            await process.StandardInput.BaseStream.FlushAsync();
        }
        catch (Exception exception)
        {
            KillProcessTree(process);
            var error = await ReadErrorTextAsync(errorTask);
            if (exception is OperationCanceledException)
            {
                throw;
            }

            throw new IOException(
                $"写入设备失败：{(string.IsNullOrWhiteSpace(error) ? exception.Message : error)}",
                exception);
        }
        finally
        {
            process.StandardInput.Close();
        }

        try
        {
            // 本地字节已全部写完后,设备侧 dd 仍在把缓冲数据写盘并 conv=fsync
            // (可达几十秒);这段时间取消必须可观察并杀掉 adb 进程,否则停止
            // 按钮失效、adb.exe 泄漏、设备侧命令继续执行。
            await process.WaitForExitAsync(cancellationToken);
        }
        catch (OperationCanceledException)
        {
            KillProcessTree(process);
            throw;
        }

        ThrowForFailure(process.ExitCode, await errorTask, string.Empty);
    }

    private static Process Start(string executable, IReadOnlyList<string> arguments, bool redirectStandardInput, bool redirectStandardOutput)
    {
        var startInfo = new ProcessStartInfo
        {
            FileName = executable,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardInput = redirectStandardInput,
            RedirectStandardOutput = redirectStandardOutput,
            RedirectStandardError = true,
            StandardOutputEncoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
            StandardErrorEncoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false)
        };

        foreach (var argument in arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        return Process.Start(startInfo) ?? throw new InvalidOperationException($"无法启动 {Path.GetFileName(executable)}。");
    }

    private static void KillProcessTree(Process process)
    {
        try
        {
            process.Kill(entireProcessTree: true);
            process.WaitForExit();
        }
        catch
        {
            // Process already exited or could not be terminated; cleanup is best effort.
        }
    }

    private static async Task<string> ReadErrorTextAsync(Task<string> errorTask)
    {
        try
        {
            return (await errorTask).Trim();
        }
        catch
        {
            return string.Empty;
        }
    }

    private static async Task CopyWithProgressAsync(Stream source, Stream destination, IProgress<PartitionTransferProgress>? progress, CancellationToken cancellationToken)
    {
        var startedAt = Stopwatch.StartNew();
        var buffer = new byte[BufferSize];
        long transferred = 0;
        int read;
        while ((read = await source.ReadAsync(buffer.AsMemory(0, buffer.Length), cancellationToken)) > 0)
        {
            await destination.WriteAsync(buffer.AsMemory(0, read), cancellationToken);
            transferred += read;
            var seconds = Math.Max(startedAt.Elapsed.TotalSeconds, 0.001);
            progress?.Report(new PartitionTransferProgress(string.Empty, transferred, null, transferred / seconds));
        }
    }

    private static void ThrowForFailure(int exitCode, string error, string output)
    {
        if (exitCode == 0)
        {
            return;
        }

        var detail = string.Join(Environment.NewLine, new[] { error, output }.Where(value => !string.IsNullOrWhiteSpace(value))).Trim();
        throw new InvalidOperationException($"adb 执行失败（退出码 {exitCode}）：{(string.IsNullOrWhiteSpace(detail) ? "未返回诊断信息。" : detail)}");
    }
}
