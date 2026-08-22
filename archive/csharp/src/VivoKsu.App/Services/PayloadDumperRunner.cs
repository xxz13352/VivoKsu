using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// Wraps the bundled payload_dumper.exe (rhythmcache/payload-dumper-rust) which
/// extracts partition images from Android OTA payload.bin files — locally or from
/// a remote URL via HTTP Range requests, without downloading the whole archive.
/// </summary>
public sealed class PayloadDumperRunner
{
    private const int ProcessTimeoutMilliseconds = 120_000;
    private readonly string executablePath;

    public PayloadDumperRunner(string executablePath)
    {
        this.executablePath = executablePath;
    }

    public bool IsAvailable => File.Exists(executablePath);

    /// <summary>payload 可以是本地 .bin/.zip 路径,也可以是 HTTP(S) 直链。</summary>
    public async Task<IReadOnlyList<PayloadPartitionEntry>> ListPartitionsAsync(
        string payload,
        CancellationToken cancellationToken)
    {
        var metadataDirectory = Path.Combine(
            Path.GetTempPath(), "VivoKsu", "payload-meta", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(metadataDirectory);
        try
        {
            var (exitCode, output, error) = await RunAsync(payload, ["--metadata", "-o", metadataDirectory, "--quiet"], cancellationToken);
            if (exitCode != 0)
            {
                throw new InvalidOperationException(CleanMessage(output, error));
            }

            var jsonPath = Path.Combine(metadataDirectory, "metadata.json");
            if (!File.Exists(jsonPath))
            {
                throw new InvalidOperationException("payload_dumper 未生成元数据。");
            }

            using var document = JsonDocument.Parse(File.ReadAllText(jsonPath));
            if (!document.RootElement.TryGetProperty("partitions", out var partitionsElement))
            {
                return [];
            }

            var partitions = new List<PayloadPartitionEntry>();
            foreach (var partition in partitionsElement.EnumerateArray())
            {
                var name = partition.TryGetProperty("partition_name", out var nameProperty) ? nameProperty.GetString() : null;
                if (string.IsNullOrWhiteSpace(name))
                {
                    continue;
                }

                var size = partition.TryGetProperty("size_in_bytes", out var sizeProperty) ? sizeProperty.GetInt64() : 0;
                var compression = partition.TryGetProperty("compression_type", out var compressionProperty)
                    ? compressionProperty.GetString() ?? "none"
                    : "none";
                partitions.Add(new PayloadPartitionEntry(name, size, compression));
            }

            return partitions;
        }
        finally
        {
            try
            {
                Directory.Delete(metadataDirectory, recursive: true);
            }
            catch
            {
                // Best effort; the temp directory is swept by the OS eventually.
            }
        }
    }

    public async Task<IReadOnlyList<PayloadExtractionResult>> ExtractAsync(
        string payload,
        IReadOnlyList<string> partitionNames,
        string outputDirectory,
        CancellationToken cancellationToken,
        IProgress<long>? writeBytesProgress = null)
    {
        Directory.CreateDirectory(outputDirectory);
        var arguments = new List<string>();
        if (partitionNames.Count > 0)
        {
            arguments.Add("-i");
            arguments.Add(string.Join(",", partitionNames));
        }

        arguments.Add("-o");
        arguments.Add(outputDirectory);

        var (exitCode, output, error) = await RunAsync(payload, arguments, cancellationToken, writeBytesProgress);
        if (exitCode != 0)
        {
            throw new InvalidOperationException(CleanMessage(output, error));
        }

        var results = new List<PayloadExtractionResult>(partitionNames.Count);
        foreach (var name in partitionNames)
        {
            var imagePath = Path.Combine(outputDirectory, $"{name}.img");
            if (File.Exists(imagePath))
            {
                results.Add(new PayloadExtractionResult(name, imagePath, new FileInfo(imagePath).Length));
            }
        }

        return results;
    }

    private async Task<(int ExitCode, string Output, string Error)> RunAsync(
        string payload,
        IReadOnlyList<string> arguments,
        CancellationToken cancellationToken,
        IProgress<long>? writeBytesProgress = null)
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
        startInfo.ArgumentList.Add(payload);
        foreach (var argument in arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        using var process = Process.Start(startInfo)
            ?? throw new InvalidOperationException("无法启动 payload_dumper。");
        using var cancellationRegistration = cancellationToken.Register(() =>
        {
            try
            {
                process.Kill(entireProcessTree: true);
            }
            catch
            {
                // Process already exited or could not be terminated.
            }
        });

        var outputTask = process.StandardOutput.ReadToEndAsync(cancellationToken);
        var errorTask = process.StandardError.ReadToEndAsync(cancellationToken);

        try
        {
            if (writeBytesProgress is not null)
            {
                // 大分区(数 GB 的 super/system)在慢盘上解压可合法超过 120 秒,
                // 固定墙钟超时会误杀正常解压。改用“无进展超时”:只要进程持续
                // 写入分区镜像就不中断,空闲超过阈值才判死。刷新超时是 600s,
                // 解包侧也用同样的“有进展就不杀”语义,不再有不对称硬超时。
                await WaitForExitWithIdleTimeoutAsync(process, writeBytesProgress, cancellationToken);
            }
            else
            {
                await process.WaitForExitAsync(cancellationToken)
                    .WaitAsync(TimeSpan.FromMilliseconds(ProcessTimeoutMilliseconds));
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

            throw new TimeoutException($"payload 处理超时（{ProcessTimeoutMilliseconds / 1000} 秒无进展），进程已终止。");
        }

        cancellationToken.ThrowIfCancellationRequested();
        var output = await outputTask;
        var error = await errorTask;
        return (process.ExitCode, output, error);
    }

    /// <summary>
    /// 等待进程结束,同时采样写入字节上报进度。只要写入字节在推进(正常解压),
    /// 就不断重置空闲计时,永不误杀;仅在连续无进展超过阈值时才判为挂死。
    /// </summary>
    private static async Task WaitForExitWithIdleTimeoutAsync(
        Process process,
        IProgress<long> progress,
        CancellationToken cancellationToken)
    {
        var idleWatch = Stopwatch.StartNew();
        long lastWriteBytes = -1;
        var hasActivity = false;
        while (!process.HasExited)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (GetProcessIoCounters(process.Handle, out var counters))
            {
                var current = (long)counters.WriteTransferCount;
                progress.Report(current);
                if (!hasActivity || current != lastWriteBytes)
                {
                    lastWriteBytes = current;
                    hasActivity = true;
                    idleWatch.Restart();
                }
            }

            if (idleWatch.Elapsed >= TimeSpan.FromMilliseconds(ProcessTimeoutMilliseconds))
            {
                throw new TimeoutException();
            }

            await Task.Delay(200, cancellationToken);
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

    private static string CleanMessage(string output, string error)
    {
        var detail = string.Join(
            Environment.NewLine,
            new[] { output, error }.Where(text => !string.IsNullOrWhiteSpace(text))).Trim();
        return string.IsNullOrWhiteSpace(detail) ? "payload_dumper 执行失败。" : detail;
    }
}
