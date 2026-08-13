using System.Diagnostics;
using System.IO;
using System.Text;

namespace VivoKsu.App.Services;

/// <summary>
/// 调用打包的 fastboot-rs CLI(<c>fastboot-rs.exe</c>)执行 fastboot 操作。
/// 相比 fastboot.dll 的 C ABI(失败只回粗粒度错误码),CLI 会打印可读、可操作的错误
/// (无设备 + 检查清单 / 镜像未找到 / 设备 FAIL 消息等)。
/// </summary>
public sealed class FastbootRsCliRunner
{
    private const int ProcessTimeoutMilliseconds = 600_000; // 大分区刷写可能较久

    private readonly string executablePath;

    public FastbootRsCliRunner(string executablePath)
    {
        this.executablePath = executablePath;
    }

    public bool IsAvailable => File.Exists(executablePath);

    /// <summary><c>fastboot flash &lt;分区&gt; &lt;镜像&gt;</c>。失败抛 <see cref="FastbootCliException"/>,消息含 CLI 输出。</summary>
    public async Task FlashAsync(string serial, string partition, string imagePath, CancellationToken cancellationToken)
    {
        var (exitCode, output) = await RunAsync(["-s", serial, "flash", partition, imagePath], cancellationToken);
        if (exitCode != 0)
        {
            throw new FastbootCliException(exitCode, $"刷写分区 {partition} 失败:{Environment.NewLine}{output}");
        }
    }

    /// <summary><c>fastboot getvar partition-type:&lt;name&gt;</c> —— 分区是否存在于设备。</summary>
    public async Task<bool> PartitionExistsAsync(string serial, string partition, CancellationToken cancellationToken)
    {
        var (exitCode, _) = await RunAsync(["-s", serial, "getvar", $"partition-type:{partition}"], cancellationToken);
        return exitCode == 0;
    }

    /// <summary><c>fastboot reboot</c> —— 刷写完成后重启回系统。</summary>
    public async Task RebootAsync(string serial, CancellationToken cancellationToken)
    {
        var (exitCode, output) = await RunAsync(["-s", serial, "reboot"], cancellationToken);
        if (exitCode != 0)
        {
            throw new FastbootCliException(exitCode, $"重启设备失败:{Environment.NewLine}{output}");
        }
    }

    private async Task<(int ExitCode, string Output)> RunAsync(
        IReadOnlyList<string> arguments,
        CancellationToken cancellationToken)
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
            ?? throw new InvalidOperationException("无法启动 fastboot-rs。");
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
            await process.WaitForExitAsync(cancellationToken)
                .WaitAsync(TimeSpan.FromMilliseconds(ProcessTimeoutMilliseconds));
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

            throw new TimeoutException("fastboot-rs 执行超时,进程已终止。");
        }

        cancellationToken.ThrowIfCancellationRequested();
        var output = await outputTask;
        var error = await errorTask;
        var combined = string.Join(
            Environment.NewLine,
            new[] { output, error }.Where(text => !string.IsNullOrWhiteSpace(text))).Trim();
        return (process.ExitCode, combined);
    }
}

/// <summary>fastboot-rs CLI 返回非零退出码时的异常,消息含 CLI 的可读输出。</summary>
public sealed class FastbootCliException : Exception
{
    public FastbootCliException(int exitCode, string message)
        : base(message)
    {
        ExitCode = exitCode;
    }

    public int ExitCode { get; }
}
