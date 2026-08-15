using System.IO.Compression;
using System.IO;
using System.Net.Http;
using System.Net.Http.Json;
using System.Text.Json.Serialization;

namespace VivoKsu.App.Services;

public interface IScrcpyProvisioningService
{
    Task<string> EnsureInstalledAsync(CancellationToken cancellationToken);
}

public sealed class ScrcpyProvisioningService : IScrcpyProvisioningService
{
    private static readonly Uri LatestReleaseUri = new("https://api.github.com/repos/Genymobile/scrcpy/releases/latest");
    private readonly HttpClient httpClient;
    private readonly IRemoteAssetDownloader downloader;
    private readonly SemaphoreSlim provisioningLock = new(1, 1);

    public ScrcpyProvisioningService(HttpClient? httpClient = null, string? installationRoot = null, IRemoteAssetDownloader? downloader = null)
    {
        this.httpClient = httpClient ?? new HttpClient();
        this.downloader = downloader ?? new RemoteAssetDownloader(this.httpClient);
        if (!this.httpClient.DefaultRequestHeaders.UserAgent.Any())
        {
            this.httpClient.DefaultRequestHeaders.UserAgent.ParseAdd("VivoKsu-App/1.0");
        }

        InstallationRoot = installationRoot ?? Path.Combine(ExternalResourceLocations.Root, "scrcpy");
    }

    public string InstallationRoot { get; }

    public async Task<string> EnsureInstalledAsync(CancellationToken cancellationToken)
    {
        // 清理上次崩溃/硬杀遗留的 .staging-* 目录(正常 finally 会删,硬杀会残留)。
        CleanupStaleStagingDirectories();

        var existing = FindInstalledExecutable();
        if (existing is not null)
        {
            return existing;
        }

        await provisioningLock.WaitAsync(cancellationToken);
        try
        {
            existing = FindInstalledExecutable();
            if (existing is not null)
            {
                return existing;
            }

            var release = await httpClient.GetFromJsonAsync<GitHubRelease>(LatestReleaseUri, cancellationToken)
                ?? throw new InvalidDataException("GitHub 未返回 scrcpy 发行信息。");
            var asset = release.Assets.FirstOrDefault(candidate =>
                candidate.Name.StartsWith("scrcpy-win64-v", StringComparison.OrdinalIgnoreCase) &&
                candidate.Name.EndsWith(".zip", StringComparison.OrdinalIgnoreCase))
                ?? throw new InvalidDataException("GitHub 最新 scrcpy 发行版不包含 Windows x64 ZIP 包。");

            if (!Uri.TryCreate(asset.DownloadUrl, UriKind.Absolute, out var assetUri) || assetUri.Scheme != Uri.UriSchemeHttps)
            {
                throw new InvalidDataException("GitHub 返回的 scrcpy 下载地址无效。");
            }

            var stagingDirectory = Path.Combine(InstallationRoot, $".staging-{Guid.NewGuid():N}");
            var archivePath = Path.Combine(stagingDirectory, asset.Name);
            var payloadDirectory = Path.Combine(stagingDirectory, "payload");
            try
            {
                Directory.CreateDirectory(stagingDirectory);
                // 走多镜像 failover 下载器(直连 github 国内不稳,镜像兜底)。
                await downloader.DownloadAsync(
                    new RemoteAssetSpec("scrcpy", assetUri.ToString()),
                    archivePath,
                    progress: null,
                    cancellationToken);

                ExtractArchiveSafely(archivePath, payloadDirectory);
                var executable = FindExecutable(payloadDirectory)
                    ?? throw new InvalidDataException("下载的 scrcpy Windows 包中未找到 scrcpy.exe。");
                var relativeExecutable = Path.GetRelativePath(payloadDirectory, executable);
                var packageDirectory = Path.Combine(InstallationRoot, Path.GetFileNameWithoutExtension(asset.Name));
                Directory.CreateDirectory(InstallationRoot);
                CopyDirectory(payloadDirectory, packageDirectory);
                return Path.Combine(packageDirectory, relativeExecutable);
            }
            finally
            {
                if (Directory.Exists(stagingDirectory))
                {
                    Directory.Delete(stagingDirectory, true);
                }
            }
        }
        finally
        {
            provisioningLock.Release();
        }
    }

    private string? FindInstalledExecutable() => FindExecutable(InstallationRoot, skipStagingDirectories: true);

    private void CleanupStaleStagingDirectories()
    {
        if (!Directory.Exists(InstallationRoot))
        {
            return;
        }

        foreach (var directory in Directory.GetDirectories(InstallationRoot, ".staging-*", SearchOption.TopDirectoryOnly))
        {
            try
            {
                Directory.Delete(directory, true);
            }
            catch
            {
                // Best effort; 下次调用继续尝试。
            }
        }
    }

    private static string? FindExecutable(string root, bool skipStagingDirectories = false)
    {
        if (!Directory.Exists(root))
        {
            return null;
        }

        var candidates = Directory.EnumerateFiles(root, "scrcpy.exe", SearchOption.AllDirectories);
        if (skipStagingDirectories)
        {
            // 只认正式安装目录里的 scrcpy.exe;跳过 .staging-* 临时目录(崩溃残留),
            // 否则残留会被当成“已安装”而永远不被清理,安装指向不稳定路径。
            candidates = candidates.Where(path => !IsInsideStaging(path));
        }

        return candidates.FirstOrDefault(path => new FileInfo(path).Length > 0);
    }

    private static bool IsInsideStaging(string path) =>
        path.Split(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)
            .Any(segment => segment.StartsWith(".staging-", StringComparison.Ordinal));

    private static void ExtractArchiveSafely(string archivePath, string destinationRoot)
    {
        var fullDestinationRoot = Path.GetFullPath(destinationRoot) + Path.DirectorySeparatorChar;
        using var archive = ZipFile.OpenRead(archivePath);
        foreach (var entry in archive.Entries)
        {
            var destinationPath = Path.GetFullPath(Path.Combine(destinationRoot, entry.FullName));
            if (!destinationPath.StartsWith(fullDestinationRoot, StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidDataException("scrcpy ZIP 包包含非法路径。");
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

    private static void CopyDirectory(string sourceDirectory, string destinationDirectory)
    {
        Directory.CreateDirectory(destinationDirectory);
        foreach (var sourceFile in Directory.GetFiles(sourceDirectory))
        {
            File.Copy(sourceFile, Path.Combine(destinationDirectory, Path.GetFileName(sourceFile)), overwrite: true);
        }

        foreach (var sourceSubdirectory in Directory.GetDirectories(sourceDirectory))
        {
            CopyDirectory(sourceSubdirectory, Path.Combine(destinationDirectory, Path.GetFileName(sourceSubdirectory)));
        }
    }

    private sealed record GitHubRelease(IReadOnlyList<GitHubReleaseAsset> Assets);

    private sealed record GitHubReleaseAsset(
        string Name,
        [property: JsonPropertyName("browser_download_url")] string BrowserDownloadUrl)
    {
        public string DownloadUrl => BrowserDownloadUrl;
    }
}
