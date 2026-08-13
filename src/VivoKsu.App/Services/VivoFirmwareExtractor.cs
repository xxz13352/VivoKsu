using System.IO;
using System.IO.Compression;
using System.Net.Http;

namespace VivoKsu.App.Services;

/// <summary>
/// Streams a Vivo OTA firmware package, which is a gzip-compressed tar archive
/// containing partition images (super.img, vbmeta_oem_*.img, ...). The whole
/// package must be streamed (gzip is not seekable), so progress is reported as
/// processed bytes over the total gzip size — a real, continuously updating value.
/// </summary>
public sealed class VivoFirmwareExtractor
{
    public readonly record struct VivoFirmwareEntry(string Name, string FullPath, long SizeBytes);

    public readonly record struct VivoProgress(long ProcessedBytes, long TotalBytes, string? CurrentEntry)
    {
        public double Fraction => TotalBytes > 0 ? Math.Clamp(ProcessedBytes / (double)TotalBytes, 0, 1) : 0;
    }

    /// <summary>
    /// If <paramref name="source"/> is an http(s) URL, streams it to a temporary gzip
    /// file (reporting download progress) and returns that path. Local paths pass through.
    /// </summary>
    public async Task<string> PrepareGzipAsync(
        string source,
        IProgress<VivoProgress>? progress,
        CancellationToken cancellationToken)
    {
        if (!Uri.TryCreate(source, UriKind.Absolute, out var uri) || uri.Scheme is not ("http" or "https"))
        {
            return source;
        }

        var tempPath = Path.Combine(Path.GetTempPath(), "VivoKsu", "vivo-firmware", Guid.NewGuid().ToString("N") + ".gz");
        Directory.CreateDirectory(Path.GetDirectoryName(tempPath)!);
        try
        {
            using var client = new HttpClient();
            using var response = await client.GetAsync(uri, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
            response.EnsureSuccessStatusCode();
            var total = response.Content.Headers.ContentLength ?? 0;
            await using var input = await response.Content.ReadAsStreamAsync(cancellationToken);
            await using var output = File.Create(tempPath);
            var buffer = new byte[1 << 20];
            long done = 0;
            while (true)
            {
                var read = await input.ReadAsync(buffer, cancellationToken);
                if (read == 0)
                {
                    break;
                }

                await output.WriteAsync(buffer.AsMemory(0, read), cancellationToken);
                done += read;
                progress?.Report(new VivoProgress(done, total, null));
            }

            return tempPath;
        }
        catch
        {
            try
            {
                File.Delete(tempPath);
            }
            catch
            {
                // Best effort.
            }

            throw;
        }
    }

    public async Task<IReadOnlyList<VivoFirmwareEntry>> ListAsync(
        string gzipPath,
        IProgress<VivoProgress>? progress,
        CancellationToken cancellationToken)
    {
        var entries = new List<VivoFirmwareEntry>();
        await foreach (var (fullPath, size, isFile) in EnumerateAsync(gzipPath, progress, cancellationToken))
        {
            if (isFile && (fullPath.EndsWith(".img", StringComparison.OrdinalIgnoreCase) ||
                           fullPath.EndsWith(".bin", StringComparison.OrdinalIgnoreCase)))
            {
                entries.Add(new VivoFirmwareEntry(Path.GetFileName(fullPath), fullPath, size));
            }
        }

        return entries;
    }

    public async Task<IReadOnlyList<(string EntryName, string OutputPath, long SizeBytes)>> ExtractAsync(
        string gzipPath,
        IReadOnlyList<VivoFirmwareEntry> selected,
        string outputDirectory,
        IProgress<VivoProgress>? progress,
        CancellationToken cancellationToken)
    {
        Directory.CreateDirectory(outputDirectory);
        var selectedPaths = new HashSet<string>(selected.Select(entry => entry.FullPath), StringComparer.OrdinalIgnoreCase);
        var results = new List<(string EntryName, string OutputPath, long SizeBytes)>();
        var gzipTotal = new FileInfo(gzipPath).Length;

        await using var file = OpenGzip(gzipPath);
        await using var gzip = new GZipStream(file, CompressionMode.Decompress);
        var header = new byte[512];
        string? pendingLongName = null;
        var buffer = new byte[1 << 20];
        string? currentEntry = null;

        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            progress?.Report(new VivoProgress(file.Position, gzipTotal, currentEntry));
            if (!await ReadExactlyAsync(gzip, header, cancellationToken))
            {
                break;
            }

            if (header.All(value => value == 0))
            {
                break;
            }

            var size = ParseOctal(header, 124, 12);
            var typeFlag = (char)header[156];
            if (typeFlag == 'L')
            {
                var nameBytes = new byte[size];
                await ReadExactlyAsync(gzip, nameBytes, cancellationToken);
                pendingLongName = System.Text.Encoding.UTF8.GetString(nameBytes).TrimEnd('\0');
                await SkipAsync(gzip, PaddedSize(size) - size);
                continue;
            }

            if (typeFlag is 'x' or 'X' or 'g')
            {
                await SkipAsync(gzip, PaddedSize(size));
                continue;
            }

            var fullPath = pendingLongName ?? GetString(header, 0, 100);
            pendingLongName = null;
            var isFile = typeFlag is '0' or '\0' or ' ' or '7';
            if (!isFile)
            {
                await SkipAsync(gzip, PaddedSize(size));
                continue;
            }

            if (selectedPaths.Contains(fullPath))
            {
                currentEntry = Path.GetFileName(fullPath);
                var outputPath = Path.Combine(outputDirectory, Path.GetFileName(fullPath));
                var remaining = size;
                long written = 0;
                await using (var output = File.Create(outputPath))
                {
                    while (remaining > 0)
                    {
                        var toRead = (int)Math.Min(buffer.Length, remaining);
                        var read = await gzip.ReadAsync(buffer.AsMemory(0, toRead), cancellationToken);
                        if (read == 0)
                        {
                            break;
                        }

                        await output.WriteAsync(buffer.AsMemory(0, read), cancellationToken);
                        remaining -= read;
                        written += read;
                        progress?.Report(new VivoProgress(file.Position, gzipTotal, currentEntry));
                    }
                }

                results.Add((Path.GetFileName(fullPath), outputPath, written));
                await SkipAsync(gzip, PaddedSize(size) - size);
            }
            else
            {
                await SkipAsync(gzip, PaddedSize(size));
            }
        }

        progress?.Report(new VivoProgress(gzipTotal, gzipTotal, null));
        return results;
    }

    private static FileStream OpenGzip(string gzipPath) =>
        new(gzipPath, FileMode.Open, FileAccess.Read, FileShare.Read, 1 << 20, useAsync: true);

    /// <summary>Streams gzip→tar, yielding (fullPath, size, isFile) for each entry.</summary>
    private static async IAsyncEnumerable<(string FullPath, long Size, bool IsFile)> EnumerateAsync(
        string gzipPath,
        IProgress<VivoProgress>? progress,
        [System.Runtime.CompilerServices.EnumeratorCancellation] CancellationToken cancellationToken)
    {
        await using var file = OpenGzip(gzipPath);
        await using var gzip = new GZipStream(file, CompressionMode.Decompress);
        var header = new byte[512];
        string? pendingLongName = null;

        while (true)
        {
            cancellationToken.ThrowIfCancellationRequested();
            progress?.Report(new VivoProgress(file.Position, file.Length, null));
            if (!await ReadExactlyAsync(gzip, header, cancellationToken))
            {
                yield break;
            }

            if (header.All(value => value == 0))
            {
                yield break;
            }

            var size = ParseOctal(header, 124, 12);
            var typeFlag = (char)header[156];

            if (typeFlag == 'L') // GNU long name
            {
                var nameBytes = new byte[size];
                await ReadExactlyAsync(gzip, nameBytes, cancellationToken);
                pendingLongName = System.Text.Encoding.UTF8.GetString(nameBytes).TrimEnd('\0');
                await SkipAsync(gzip, PaddedSize(size) - size);
                continue;
            }

            if (typeFlag is 'x' or 'X' or 'g') // pax extended headers
            {
                await SkipAsync(gzip, PaddedSize(size));
                continue;
            }

            var fullPath = pendingLongName ?? GetString(header, 0, 100);
            pendingLongName = null;
            var isFile = typeFlag is '0' or '\0' or ' ' or '7';
            if (!isFile)
            {
                await SkipAsync(gzip, PaddedSize(size));
                continue;
            }

            yield return (fullPath, size, true);
            await SkipAsync(gzip, PaddedSize(size));
        }
    }

    private static async Task SkipAsync(Stream stream, long count)
    {
        var buffer = new byte[8192];
        while (count > 0)
        {
            var toRead = (int)Math.Min(buffer.Length, count);
            var read = await stream.ReadAsync(buffer.AsMemory(0, toRead));
            if (read == 0)
            {
                break;
            }

            count -= read;
        }
    }

    private static async Task<bool> ReadExactlyAsync(Stream stream, byte[] buffer, CancellationToken cancellationToken)
    {
        var offset = 0;
        while (offset < buffer.Length)
        {
            var read = await stream.ReadAsync(buffer.AsMemory(offset, buffer.Length - offset), cancellationToken);
            if (read == 0)
            {
                return false;
            }

            offset += read;
        }

        return true;
    }

    private static long PaddedSize(long size) => size + ((512 - (size % 512)) % 512);

    private static long ParseOctal(byte[] header, int offset, int length)
    {
        var text = System.Text.Encoding.ASCII.GetString(header, offset, length).Trim('\0', ' ');
        if (text.Length == 0)
        {
            return 0;
        }

        // Some tar writers leave stray bytes in the field; keep only octal digits.
        var digits = new string(text.Where(character => character is >= '0' and <= '7').ToArray());
        return digits.Length == 0 ? 0 : Convert.ToInt64(digits, 8);
    }

    private static string GetString(byte[] header, int offset, int length) =>
        System.Text.Encoding.UTF8.GetString(header, offset, length).TrimEnd('\0').Trim();
}

public static class FirmwareFormatDetector
{
    public enum FirmwareKind
    {
        Unknown,
        PayloadZip,
        VivoGzip
    }

    /// <summary>Peeks the first bytes of a local path or http(s) URL to classify the format.</summary>
    public static async Task<FirmwareKind> DetectAsync(string source, CancellationToken cancellationToken)
    {
        var magic = await PeekAsync(source, 4, cancellationToken);
        if (magic.Length >= 2 && magic[0] == 0x1f && magic[1] == 0x8b)
        {
            return FirmwareKind.VivoGzip;
        }

        if (magic.Length >= 2 && magic[0] == 0x50 && magic[1] == 0x4b)
        {
            return FirmwareKind.PayloadZip;
        }

        if (magic.Length >= 4 && magic[0] == 0x43 && magic[1] == 0x72 && magic[2] == 0x41 && magic[3] == 0x55)
        {
            return FirmwareKind.PayloadZip;
        }

        return FirmwareKind.Unknown;
    }

    private static async Task<byte[]> PeekAsync(string source, int count, CancellationToken cancellationToken)
    {
        if (Uri.TryCreate(source, UriKind.Absolute, out var uri) && uri.Scheme is "http" or "https")
        {
            using var client = new HttpClient();
            using var request = new HttpRequestMessage(HttpMethod.Get, uri);
            request.Headers.Range = new System.Net.Http.Headers.RangeHeaderValue(0, count - 1);
            using var response = await client.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
            response.EnsureSuccessStatusCode();
            return await response.Content.ReadAsByteArrayAsync(cancellationToken);
        }

        await using var file = File.OpenRead(source);
        var buffer = new byte[count];
        var read = await file.ReadAsync(buffer, cancellationToken);
        return buffer[..read];
    }
}
