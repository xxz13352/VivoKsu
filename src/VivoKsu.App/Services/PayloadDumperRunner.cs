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

        // payload_dumper does not stream progress and pre-allocates output files, so the only
        // real signal is how many bytes it has actually written to the partition images. Its
        // network reads (Rust reqwest over IOCP/AFD) do not show up as read I/O, but the file
        // writes do — sample the write counter on a background thread so the UI stays live.
        var samplerTask = writeBytesProgress is null
            ? null
            : Task.Run(() => SampleWriteBytesLoop(process, writeBytesProgress, cancellationToken));

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

            throw new TimeoutException("payload 处理超时（120 秒），进程已终止。");
        }
        finally
        {
            if (samplerTask is not null)
            {
                try
                {
                    await samplerTask;
                }
                catch
                {
                    // Best effort; sampling is optional.
                }
            }
        }

        cancellationToken.ThrowIfCancellationRequested();
        var output = await outputTask;
        var error = await errorTask;
        return (process.ExitCode, output, error);
    }

    private static async Task SampleWriteBytesLoop(
        Process process,
        IProgress<long> progress,
        CancellationToken cancellationToken)
    {
        try
        {
            while (true)
            {
                if (GetProcessIoCounters(process.Handle, out var counters))
                {
                    progress.Report((long)counters.WriteTransferCount);
                }

                if (process.HasExited)
                {
                    break;
                }

                await Task.Delay(200, cancellationToken);
            }
        }
        catch (OperationCanceledException)
        {
            // Cancelled.
        }
        catch
        {
            // Process disposed or handle unavailable — sampling is best effort.
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
