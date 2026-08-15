using System.IO;
using System.IO.Compression;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>安全刷写中一个已解出的待刷分区镜像。</summary>
public sealed record SafeFlashImageInfo(string PartitionName, string ImagePath, long SizeBytes);

/// <summary>
/// 从 OTA zip / payload.bin 列出并解出安全刷写要刷的分区镜像。
/// 支持两类源:
///  1. payload 类 —— 含 payload.bin 的 OTA zip,或裸 payload.bin / CrAU,用 payload_dumper 按需解出;
///  2. 直接镜像 zip —— 如 MTK PD2057 的 OTA,zip 里直接是 *.img / *.bin,用 ZipFile 解出。
/// 始终过滤 preloader* 与 lk(安全刷写只跳过引导加载器,其余全刷)。
/// </summary>
public sealed class FirmwarePartitionExtractor
{
    private static readonly string[] ImageExtensions = [".img", ".bin"];

    private PayloadDumperRunner? payloadDumper;
    private readonly PayloadDumperProvisioner? provisioner;

    public FirmwarePartitionExtractor(PayloadDumperRunner? payloadDumper, PayloadDumperProvisioner? provisioner = null)
    {
        this.payloadDumper = payloadDumper;
        this.provisioner = provisioner;
    }

    /// <summary>取得可用的 payload_dumper runner;未就绪(发布版不随包)时经 provisioner 按需下载。</summary>
    private async Task<PayloadDumperRunner> AcquirePayloadDumperAsync(CancellationToken cancellationToken)
    {
        if (payloadDumper?.IsAvailable == true)
        {
            return payloadDumper;
        }

        if (provisioner is null)
        {
            throw new InvalidOperationException("payload 提取器未就绪。");
        }

        payloadDumper = await provisioner.EnsureInstalledAsync(cancellationToken);
        return payloadDumper;
    }

    /// <summary>安全刷写要跳过的分区:lk 与 preloader*。其余全部刷入。</summary>
    public static bool ShouldSkip(string partitionName) =>
        partitionName.Equals("lk", StringComparison.OrdinalIgnoreCase) ||
        partitionName.Contains("preloader", StringComparison.OrdinalIgnoreCase);

    /// <summary>源是否为 payload 类(含 payload.bin 的 zip 或裸 payload.bin)。</summary>
    public static bool IsPayloadSource(string source) =>
        !IsZip(source) || ContainsPayloadBin(source);

    /// <summary>源是否为「已解包的镜像文件夹」(用户选择 / 设备断开后保留的提取目录)。</summary>
    public static bool IsDirectorySource(string source) => Directory.Exists(source);

    /// <summary>列出源中的待刷分区(已过滤 preloader*/lk)。</summary>
    public async Task<IReadOnlyList<PayloadPartitionEntry>> ListPartitionsAsync(
        string source,
        CancellationToken cancellationToken)
    {
        if (IsDirectorySource(source))
        {
            return ListDirectoryImages(source)
                .Where(entry => !ShouldSkip(entry.Name))
                .ToList();
        }

        var kind = await FirmwareFormatDetector.DetectAsync(source, cancellationToken);
        if (kind == FirmwareFormatDetector.FirmwareKind.VivoGzip)
        {
            throw new InvalidDataException("不支持 Vivo gzip 固件格式,请选择 payload.bin 或 OTA zip。");
        }

        if (kind != FirmwareFormatDetector.FirmwareKind.PayloadZip)
        {
            throw new InvalidDataException("无法识别的固件格式(支持 payload.bin / 含 payload.bin 的 OTA zip / 直接镜像 zip)。");
        }

        if (!IsPayloadSource(source))
        {
            return ListImageEntries(source)
                .Where(entry => !ShouldSkip(Path.GetFileNameWithoutExtension(entry.FullName)))
                .Select(entry => new PayloadPartitionEntry(
                    Path.GetFileNameWithoutExtension(entry.FullName),
                    entry.Length,
                    "store"))
                .ToList();
        }

        if (payloadDumper is null)
        {
            payloadDumper = await AcquirePayloadDumperAsync(cancellationToken);
        }

        var partitions = await payloadDumper.ListPartitionsAsync(source, cancellationToken);
        return partitions.Where(partition => !ShouldSkip(partition.Name)).ToList();
    }

    /// <summary>解出单个分区镜像到 outputDirectory,报告该分区的写入字节数。</summary>
    public async Task<SafeFlashImageInfo> ExtractPartitionAsync(
        string source,
        string partitionName,
        string outputDirectory,
        CancellationToken cancellationToken,
        IProgress<long>? writeProgress = null)
    {
        Directory.CreateDirectory(outputDirectory);
        if (IsDirectorySource(source))
        {
            // 已解包的文件夹源:镜像就在源目录,直接引用不复制(避免再拷贝数 GB)。
            var imagePath = FindDirectoryImage(source, partitionName);
            return new SafeFlashImageInfo(partitionName, imagePath, new FileInfo(imagePath).Length);
        }

        if (IsPayloadSource(source))
        {
            if (payloadDumper is null)
            {
                payloadDumper = await AcquirePayloadDumperAsync(cancellationToken);
            }

            var results = await payloadDumper.ExtractAsync(
                source, [partitionName], outputDirectory, cancellationToken, writeProgress);
            var result = results.SingleOrDefault()
                ?? throw new InvalidDataException($"payload 中未解出分区 {partitionName}。");
            return new SafeFlashImageInfo(partitionName, result.OutputPath, result.SizeBytes);
        }

        return await ExtractImageFromZipAsync(source, partitionName, outputDirectory, cancellationToken, writeProgress);
    }

    private static async Task<SafeFlashImageInfo> ExtractImageFromZipAsync(
        string source,
        string partitionName,
        string outputDirectory,
        CancellationToken cancellationToken,
        IProgress<long>? writeProgress)
    {
        using var archive = ZipFile.OpenRead(source);
        var entry = archive.Entries.FirstOrDefault(candidate =>
            !candidate.FullName.EndsWith('/') &&
            string.Equals(
                Path.GetFileNameWithoutExtension(candidate.FullName),
                partitionName,
                StringComparison.OrdinalIgnoreCase))
            ?? throw new InvalidDataException($"固件包内未找到分区 {partitionName} 的镜像。");

        var outputPath = Path.Combine(outputDirectory, $"{partitionName}.img");
        long total = entry.Length;
        long done = 0;
        await using (var input = entry.Open())
        await using (var output = new FileStream(outputPath, FileMode.Create, FileAccess.Write, FileShare.None, 1 << 20, useAsync: true))
        {
            var buffer = new byte[1 << 20];
            int read;
            while ((read = await input.ReadAsync(buffer, cancellationToken)) > 0)
            {
                await output.WriteAsync(buffer.AsMemory(0, read), cancellationToken);
                done += read;
                writeProgress?.Report(done);
            }
        }

        return new SafeFlashImageInfo(partitionName, outputPath, done);
    }

    /// <summary>
    /// 直接镜像 zip 里是否存在块式分区内容(.new.dat / .patch.dat / .transfer.list)。
    /// 块式(如 PD2057 的 system/vendor/product)暂不支持刷写,会被 ListImageEntries
    /// 静默剔除;上层必须向用户显式警告,否则用户误以为全量刷写 → 新旧固件混搭。
    /// </summary>
    public bool HasBlockBasedContent(string source)
    {
        if (IsDirectorySource(source))
        {
            // 已解包文件夹只含可直接刷的镜像,无块式内容。
            return false;
        }

        if (IsPayloadSource(source))
        {
            return false;
        }

        using var archive = ZipFile.OpenRead(source);
        return archive.Entries.Any(entry =>
            entry.Name.EndsWith(".new.dat", StringComparison.OrdinalIgnoreCase) ||
            entry.Name.EndsWith(".patch.dat", StringComparison.OrdinalIgnoreCase) ||
            entry.Name.EndsWith(".transfer.list", StringComparison.OrdinalIgnoreCase));
    }

    /// <summary>解包文件夹里的可刷镜像:顶层 *.img / *.bin(排除 payload.bin)。</summary>
    private static IEnumerable<PayloadPartitionEntry> ListDirectoryImages(string source)
    {
        foreach (var file in Directory.EnumerateFiles(source, "*.*", SearchOption.TopDirectoryOnly))
        {
            var extension = Path.GetExtension(file);
            if (!ImageExtensions.Contains(extension, StringComparer.OrdinalIgnoreCase))
            {
                continue;
            }

            var name = Path.GetFileNameWithoutExtension(file);
            if (name.Equals("payload", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            yield return new PayloadPartitionEntry(name, new FileInfo(file).Length, "store");
        }
    }

    /// <summary>按分区名在解包文件夹里定位镜像(支持 .img/.bin 扩展名)。</summary>
    private static string FindDirectoryImage(string source, string partitionName)
    {
        foreach (var file in Directory.EnumerateFiles(source, "*.*", SearchOption.TopDirectoryOnly))
        {
            var extension = Path.GetExtension(file);
            if (!ImageExtensions.Contains(extension, StringComparer.OrdinalIgnoreCase))
            {
                continue;
            }

            if (string.Equals(Path.GetFileNameWithoutExtension(file), partitionName, StringComparison.OrdinalIgnoreCase))
            {
                return file;
            }
        }

        throw new InvalidDataException($"解包文件夹内未找到分区 {partitionName} 的镜像。");
    }

    /// <summary>zip 里的可刷镜像条目:顶层 *.img / *.bin(排除 payload.bin 与 META-INF)。</summary>
    private static IEnumerable<ZipArchiveEntry> ListImageEntries(string source)
    {
        using var archive = ZipFile.OpenRead(source);
        foreach (var entry in archive.Entries)
        {
            if (entry.FullName.EndsWith('/'))
            {
                continue;
            }

            var extension = Path.GetExtension(entry.Name);
            if (!ImageExtensions.Contains(extension, StringComparer.OrdinalIgnoreCase))
            {
                continue;
            }

            if (entry.Name.Equals("payload.bin", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            yield return entry;
        }
    }

    private static bool IsZip(string source) =>
        source.StartsWith("PK", StringComparison.Ordinal) ||
        (File.Exists(source) && HasZipMagic(source));

    private static bool HasZipMagic(string source)
    {
        using var stream = File.OpenRead(source);
        Span<byte> magic = stackalloc byte[2];
        if (stream.Read(magic) < 2)
        {
            return false;
        }

        return magic[0] == 0x50 && magic[1] == 0x4B;
    }

    private static bool ContainsPayloadBin(string source)
    {
        using var archive = ZipFile.OpenRead(source);
        return archive.Entries.Any(entry =>
            entry.Name.Equals("payload.bin", StringComparison.OrdinalIgnoreCase));
    }
}
