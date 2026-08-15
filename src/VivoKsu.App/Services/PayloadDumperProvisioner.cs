using System.IO;
using System.IO.Compression;
using System.Security.Cryptography;

namespace VivoKsu.App.Services;

/// <summary>
/// payload_dumper(OTA 解包工具)定位/按需下载器。发布瘦身后该工具不再随包分发
/// (原 <c>payload-tools</c>),固件提取 / 安全刷写首次使用前下载到用户缓存目录,
/// 解压后校验 SHA-256 再投入使用。开发/测试构建随包仍存在时直接用随包,不下载。
/// </summary>
public sealed class PayloadDumperProvisioner
{
    private readonly IRemoteAssetDownloader downloader;
    private readonly string? bundledExecutablePath;
    private readonly SemaphoreSlim provisioningLock = new(1, 1);

    public PayloadDumperProvisioner(
        IRemoteAssetDownloader? downloader = null,
        string? installationRoot = null,
        string? bundledExecutablePath = null)
    {
        this.downloader = downloader ?? new RemoteAssetDownloader();
        this.bundledExecutablePath = bundledExecutablePath;
        InstallationRoot = installationRoot ?? Path.Combine(ExternalResourceLocations.Root, "payload-dumper");
    }

    public string InstallationRoot { get; }

    /// <summary>按需下载后的缓存 exe 路径。</summary>
    public string CachedExecutablePath => Path.Combine(InstallationRoot, RemoteAssetCatalog.PayloadDumperExecutableName);

    /// <summary>随包 exe(存在且非空);开发/测试构建才有。</summary>
    private string? BundledExecutablePath =>
        bundledExecutablePath is { } path && File.Exists(path) && new FileInfo(path).Length > 0 ? path : null;

    /// <summary>当前生效的 exe 路径:随包存在用随包,否则缓存。</summary>
    public string ExecutablePath => BundledExecutablePath ?? CachedExecutablePath;

    public bool IsAvailable => BundledExecutablePath is not null || File.Exists(CachedExecutablePath);

    /// <summary>确保 payload_dumper 就绪并返回其 runner:随包→直接用;缓存→复用;都没有→下载→解压→SHA-256 校验→落缓存。</summary>
    public async Task<PayloadDumperRunner> EnsureInstalledAsync(CancellationToken cancellationToken)
    {
        if (BundledExecutablePath is { } bundled)
        {
            return new PayloadDumperRunner(bundled);
        }

        if (File.Exists(CachedExecutablePath))
        {
            return new PayloadDumperRunner(CachedExecutablePath);
        }

        await provisioningLock.WaitAsync(cancellationToken);
        try
        {
            if (File.Exists(CachedExecutablePath))
            {
                return new PayloadDumperRunner(CachedExecutablePath);
            }

            var stagingRoot = Path.Combine(
                Path.GetTempPath(), "VivoKsu", "payload-dumper", Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(stagingRoot);
            try
            {
                var zipPath = Path.Combine(stagingRoot, RemoteAssetCatalog.PayloadDumperAssetName);
                var spec = new RemoteAssetSpec(
                    "payload_dumper",
                    RemoteAssetCatalog.GitHubDownloadUrl(RemoteAssetCatalog.PayloadDumperAssetName));
                await downloader.DownloadAsync(spec, zipPath, progress: null, cancellationToken);

                ExtractArchiveSafely(zipPath, stagingRoot);
                var extractedExecutable = Path.Combine(stagingRoot, RemoteAssetCatalog.PayloadDumperExecutableName);
                if (!File.Exists(extractedExecutable))
                {
                    throw new InvalidDataException("payload_dumper 包中未找到 payload_dumper.exe。");
                }

                VerifyExecutableHash(extractedExecutable);
                Directory.CreateDirectory(InstallationRoot);
                File.Copy(extractedExecutable, CachedExecutablePath, overwrite: true);
                return new PayloadDumperRunner(CachedExecutablePath);
            }
            finally
            {
                TryDeleteDirectory(stagingRoot);
            }
        }
        finally
        {
            provisioningLock.Release();
        }
    }

    /// <summary>解压包内文件到目标根;逐项守卫路径穿越(与 scrcpy 解压同策略)。</summary>
    private static void ExtractArchiveSafely(string archivePath, string destinationRoot)
    {
        var fullDestinationRoot = Path.GetFullPath(destinationRoot) + Path.DirectorySeparatorChar;
        using var archive = ZipFile.OpenRead(archivePath);
        foreach (var entry in archive.Entries)
        {
            var destinationPath = Path.GetFullPath(Path.Combine(destinationRoot, entry.FullName));
            if (!destinationPath.StartsWith(fullDestinationRoot, StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidDataException("payload_dumper 包包含非法路径。");
            }

            if (string.IsNullOrEmpty(entry.Name))
            {
                Directory.CreateDirectory(destinationPath);
                continue;
            }

            Directory.CreateDirectory(Path.GetDirectoryName(destinationPath)!);
            entry.ExtractToFile(destinationPath, overwrite: true);
        }
    }

    /// <summary>解压后的 exe 必须与随包 pin 的哈希一致,防下载源被替换成恶意二进制。</summary>
    private static void VerifyExecutableHash(string path)
    {
        using var stream = File.OpenRead(path);
        var actualHash = Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
        if (!string.Equals(actualHash, RemoteAssetCatalog.PayloadDumperSha256, StringComparison.OrdinalIgnoreCase))
        {
            throw new InvalidDataException("payload_dumper 完整性校验失败(SHA-256 不匹配)。");
        }
    }

    private static void TryDeleteDirectory(string path)
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
            // Best effort; 下次下载的 staging 独立命名。
        }
    }
}
