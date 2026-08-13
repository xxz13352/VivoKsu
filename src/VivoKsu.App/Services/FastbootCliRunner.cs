using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace VivoKsu.App.Services;

/// <summary>
/// 调用统一的 fastboot 可执行文件(<c>platform-tools/fastboot.exe</c>)执行 fastboot 操作。
/// 取代 fastboot-rs 原生 DLL:
/// - 错误可读(exit code + 输出文本,不再只有粗粒度 C ABI 错误码);
/// - 刷写带连续进度(<see cref="GetProcessIoCounters"/> 采样 进程写字节 / 镜像大小)。
/// </summary>
public sealed class FastbootCliRunner : IFastbootCliRunner
{
    /// <summary>刷写类命令的“无进展”超时:只要进程还在读写镜像就绝不中断。</summary>
    private const int ProcessIdleTimeoutMilliseconds = 600_000;
    /// <summary>getvar 探测的短墙钟超时:探测不该慢,避免卡死拖住整条刷写。</summary>
    private const int ProbeTimeoutMilliseconds = 20_000;
    /// <summary>reboot / erase / set_active 的短墙钟超时。</summary>
    private const int ShortCommandTimeoutMilliseconds = 60_000;

    private readonly string executablePath;

    public FastbootCliRunner(string executablePath)
    {
        this.executablePath = executablePath;
    }

    public bool IsAvailable => File.Exists(executablePath);

    /// <summary><c>fastboot flash &lt;分区&gt; &lt;镜像&gt;</c>。带连续传输进度;失败抛 <see cref="FastbootCliException"/>,消息含 CLI 可读输出。</summary>
    public async Task FlashAsync(
        string serial,
        string partition,
        string imagePath,
        IProgress<double>? progress,
        CancellationToken cancellationToken)
    {
        var imageSize = new FileInfo(imagePath).Length;
        if (imageSize <= 0)
        {
            throw new InvalidDataException($"镜像文件为空或不可读: {imagePath}");
        }

        var (exitCode, output) = await RunAsync(
            ["-s", serial, "flash", partition, imagePath],
            cancellationToken,
            onSample: bytesWritten => progress?.Report(Math.Clamp(bytesWritten / (double)imageSize, 0d, 1d)));
        if (exitCode != 0)
        {
            throw new FastbootCliException(exitCode, $"刷写分区 {partition} 失败:{Environment.NewLine}{output}");
        }
    }

    /// <summary><c>fastboot erase &lt;分区&gt;</c>。</summary>
    public async Task EraseAsync(string serial, string partition, CancellationToken cancellationToken)
    {
        var (exitCode, output) = await RunAsync(
            ["-s", serial, "erase", partition],
            cancellationToken,
            wallClockTimeout: TimeSpan.FromMilliseconds(ShortCommandTimeoutMilliseconds));
        if (exitCode != 0)
        {
            throw new FastbootCliException(exitCode, $"擦除分区 {partition} 失败:{Environment.NewLine}{output}");
        }
    }

    /// <summary>
    /// <c>fastboot getvar &lt;变量&gt;</c>。返回剥离 <c>(bootloader)</c> 前缀后的值;
    /// <c>all</c> 返回原始多行输出(由分区表解析器处理)。失败抛 <see cref="FastbootCliException"/>。
    /// </summary>
    public async Task<string> GetVarAsync(string serial, string variable, CancellationToken cancellationToken)
    {
        var (exitCode, output) = await RunAsync(
            ["-s", serial, "getvar", variable],
            cancellationToken,
            wallClockTimeout: TimeSpan.FromMilliseconds(ProbeTimeoutMilliseconds));
        if (exitCode != 0)
        {
            throw new FastbootCliException(exitCode, $"读取变量 {variable} 失败:{Environment.NewLine}{output}");
        }

        return string.Equals(variable, "all", StringComparison.OrdinalIgnoreCase)
            ? output
            : ExtractVariableValue(output, variable);
    }

    /// <summary><c>fastboot getvar partition-type:&lt;name&gt;</c> —— 分区是否存在于设备。</summary>
    public async Task<bool> PartitionExistsAsync(string serial, string partition, CancellationToken cancellationToken)
    {
        var (exitCode, output) = await RunAsync(
            ["-s", serial, "getvar", $"partition-type:{partition}"],
            cancellationToken,
            wallClockTimeout: TimeSpan.FromMilliseconds(ProbeTimeoutMilliseconds));
        if (exitCode == 0)
        {
            return true;
        }

        // 非零退出要区分“设备无此分区”(预期跳过)与探测失败(传输/连接错误)。
        // 后者必须抛错中止,否则瞬态 getvar 失败会让必需分区被静默跳过,
        // 整条流程仍报成功、设备半刷。
        if (LooksLikeMissingPartition(output))
        {
            return false;
        }

        throw new FastbootCliException(
            exitCode,
            $"探测分区 {partition} 失败（可能是设备连接问题，而非分区不存在）:{Environment.NewLine}{output}");
    }

    /// <summary><c>fastboot reboot</c> —— 刷写完成后重启回系统。</summary>
    public async Task RebootAsync(string serial, CancellationToken cancellationToken)
    {
        var (exitCode, output) = await RunAsync(
            ["-s", serial, "reboot"],
            cancellationToken,
            wallClockTimeout: TimeSpan.FromMilliseconds(ShortCommandTimeoutMilliseconds));
        if (exitCode != 0)
        {
            throw new FastbootCliException(exitCode, $"重启设备失败:{Environment.NewLine}{output}");
        }
    }

    /// <summary><c>fastboot set_active &lt;槽位&gt;</c> —— 双槽设备切换活动槽位。</summary>
    public async Task SetActiveAsync(string serial, string slot, CancellationToken cancellationToken)
    {
        var (exitCode, output) = await RunAsync(
            ["-s", serial, "set_active", slot],
            cancellationToken,
            wallClockTimeout: TimeSpan.FromMilliseconds(ShortCommandTimeoutMilliseconds));
        if (exitCode != 0)
        {
            throw new FastbootCliException(exitCode, $"切换活动槽位失败:{Environment.NewLine}{output}");
        }
    }

    private static bool LooksLikeMissingPartition(string output) =>
        output.Contains("unknown partition", StringComparison.OrdinalIgnoreCase) ||
        output.Contains("partition not found", StringComparison.OrdinalIgnoreCase) ||
        output.Contains("does not exist", StringComparison.OrdinalIgnoreCase) ||
        output.Contains("not found", StringComparison.OrdinalIgnoreCase) ||
        output.Contains("unknown variable", StringComparison.OrdinalIgnoreCase);

    /// <summary>从 <c>fastboot getvar</c> 输出提取单个变量的值(剥离 bootloader 前缀)。</summary>
    private static string ExtractVariableValue(string output, string variable)
    {
        var prefix = $"{variable}:";
        foreach (var sourceLine in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            var line = NormalizeLine(sourceLine);
            if (!line.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            return line[prefix.Length..].Trim();
        }

        // 某些 fastboot 对成功 getvar 直接回裸值(无 "var:" 前缀),原样返回。
        return output.Trim();
    }

    private static string NormalizeLine(string sourceLine)
    {
        var line = sourceLine.Trim();
        const string bootloaderPrefix = "(bootloader)";
        if (line.StartsWith(bootloaderPrefix, StringComparison.OrdinalIgnoreCase))
        {
            line = line[bootloaderPrefix.Length..].TrimStart();
        }

        return line.StartsWith("INFO", StringComparison.OrdinalIgnoreCase)
            ? line["INFO".Length..].TrimStart()
            : line;
    }

    private async Task<(int ExitCode, string Output)> RunAsync(
        IReadOnlyList<string> arguments,
        CancellationToken cancellationToken,
        TimeSpan? wallClockTimeout = null,
        Action<ulong>? onSample = null)
    {
        var startInfo = new ProcessStartInfo
        {
            FileName = executablePath,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = new UTF8Encoding(false),
            StandardErrorEncoding = new UTF8Encoding(false)
        };
        foreach (var argument in arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        using var process = Process.Start(startInfo)
            ?? throw new InvalidOperationException("无法启动 fastboot。");
        using var registration = cancellationToken.Register(() =>
        {
            try
            {
                process.Kill(entireProcessTree: true);
            }
            catch
            {
                // 进程已退出或无法终止。
            }
        });

        var outputTask = process.StandardOutput.ReadToEndAsync(cancellationToken);
        var errorTask = process.StandardError.ReadToEndAsync(cancellationToken);
        try
        {
            if (wallClockTimeout is not null)
            {
                await process.WaitForExitAsync(cancellationToken).WaitAsync(wallClockTimeout.Value);
            }
            else
            {
                // flash 用“无进展超时”:只要进程还在读写镜像(读图 + 写 USB)就不断
                // 重置空闲计时,慢速 USB 刷 4-6GB 大分区不再被 600s 墙钟强杀中断。
                // 同一采样循环里把已写字节数回调给调用方算进度。
                await WaitForExitWithIdleTimeoutAsync(process, cancellationToken, onSample);
            }
        }
        catch (TimeoutException)
        {
            try
            {
                process.Kill(entireProcessTree: true);
            }
            catch
            {
                // Best effort.
            }

            throw new TimeoutException("fastboot 执行超时,进程已终止。");
        }

        cancellationToken.ThrowIfCancellationRequested();
        var output = await outputTask;
        var error = await errorTask;
        var combined = string.Join(
            Environment.NewLine,
            new[] { output, error }.Where(text => !string.IsNullOrWhiteSpace(text))).Trim();
        return (process.ExitCode, combined);
    }

    private static async Task WaitForExitWithIdleTimeoutAsync(Process process, CancellationToken cancellationToken, Action<ulong>? onSample = null)
    {
        var idleWatch = Stopwatch.StartNew();
        ulong lastActivity = 0;
        var hasActivity = false;
        while (!process.HasExited)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (GetProcessIoCounters(process.Handle, out var counters))
            {
                var current = counters.ReadTransferCount + counters.WriteTransferCount;
                if (!hasActivity || current != lastActivity)
                {
                    lastActivity = current;
                    hasActivity = true;
                    idleWatch.Restart();
                }

                onSample?.Invoke(counters.WriteTransferCount);
            }

            if (idleWatch.Elapsed >= TimeSpan.FromMilliseconds(ProcessIdleTimeoutMilliseconds))
            {
                throw new TimeoutException();
            }

            await Task.Delay(250, cancellationToken);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct IoCounters
    {
        public ulong ReadOperationCount;
        public ulong WriteOperationCount;
        public ulong OtherOperationCount;
        public ulong ReadTransferCount;
        public ulong WriteTransferCount;
        public ulong OtherTransferCount;
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetProcessIoCounters(IntPtr processHandle, out IoCounters counters);
}

/// <summary>fastboot CLI 返回非零退出码时的异常,消息含 CLI 的可读输出。</summary>
public sealed class FastbootCliException : Exception
{
    public FastbootCliException(int exitCode, string message)
        : base(message)
    {
        ExitCode = exitCode;
    }

    public int ExitCode { get; }
}
