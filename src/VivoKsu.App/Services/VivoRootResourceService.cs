using System.IO.Compression;
using System.IO;
using System.Security.Cryptography;

namespace VivoKsu.App.Services;

public sealed record VivoRootLibrarySpec;

public sealed record VivoRootToolResource(string Name, string Path);

public sealed record VivoRootManagerResource(
    string Key,
    string ApkPath,
    string PackageName,
    string ActivityName,
    IReadOnlyDictionary<string, VivoRootLibrarySpec> Libraries);

public sealed class VivoRootResourceService
{
    private static readonly IReadOnlyList<string> kSupportedKmis =
        ["android13-5.15", "android14-6.1", "android15-6.6"];

    private static readonly IReadOnlyDictionary<string, VivoRootManagerResource> ManagerCatalog =
        new Dictionary<string, VivoRootManagerResource>(StringComparer.Ordinal)
        {
            ["KSU"] = new(
                "KSU", string.Empty,
                "me.inkdye.vivoksu", "me.inkdye.vivoksu.ui.MainActivity",
                new Dictionary<string, VivoRootLibrarySpec>(StringComparer.Ordinal)
                {
                    ["arm64-v8a"] = new(),
                    ["x86_64"] = new()
                }),
            ["OfficialKsu"] = new(
                "OfficialKsu", string.Empty,
                "me.weishu.kernelsu", "me.weishu.kernelsu.ui.MainActivity",
                new Dictionary<string, VivoRootLibrarySpec>(StringComparer.Ordinal)
                {
                    ["arm64-v8a"] = new(),
                    ["x86_64"] = new()
                })
        };

    private readonly string projectRoot;
    private readonly IRemoteAssetDownloader? downloader;

    /// <summary>按需下载的缓存根目录(发布瘦身后 APK 不再随包,ROOT 前下载到这里)。</summary>
    private static string CacheRoot => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "VivoKsu",
        "resources");

    private static string CacheApkPath(string key) =>
        Path.Combine(CacheRoot, "apk", ApkFileName(key));

    /// <summary>
    /// 随包分发的管理器 APK 的 SHA-256。更新 apk/ 下的 APK 后必须同步更新,
    /// 否则 ROOT 流程会拒绝安装(完整性校验失败即失败关闭)。
    /// 重新生成:certutil -hashfile &lt;apk&gt; SHA256
    /// </summary>
    private static readonly IReadOnlyDictionary<string, string> ManagerApkSha256 =
        new Dictionary<string, string>(StringComparer.Ordinal)
        {
            ["KSU"] = "43ebb3e3cbc885285bd824f351e5cca2169a4435c8bd0268584ad3c9d7248d4a",
            ["OfficialKsu"] = "dca1cf72a6f6cff4a116242fbe940a161099bafbd9d74ca4518756eaad5c8c03"
        };

    public VivoRootResourceService(string projectRoot, IRemoteAssetDownloader? downloader = null)
    {
        this.projectRoot = Path.GetFullPath(projectRoot);
        this.downloader = downloader;
    }

    public static IReadOnlyList<string> SupportedKmis => kSupportedKmis;

    public IReadOnlyList<string> ManagerKeys => ManagerCatalog.Keys.ToArray();

    public VivoRootManagerResource ResolveManager(string key)
    {
        if (!ManagerCatalog.TryGetValue(key, out var catalog))
        {
            throw new ArgumentException($"不支持的 ROOT 管理器: {key}", nameof(key));
        }

        var bundledPath = Path.Combine(projectRoot, "apk", ApkFileName(key));
        // 随包存在(开发/测试)用随包;发布版无 apk/ 目录 → 用按需下载缓存路径。
        var apkPath = File.Exists(bundledPath) ? bundledPath : CacheApkPath(key);
        return catalog with { ApkPath = apkPath };
    }

    /// <summary>管理器 APK 的文件名(远程资产名与随包文件名一致)。</summary>
    private static string ApkFileName(string key) => key == "KSU" ? "KSU.APK" : "KernelSU.apk";

    /// <summary>
    /// 确保管理器 APK 可用:随包/缓存已存在且通过完整性校验则直接返回;否则从远程
    /// 下载到缓存并做强校验(SHA-256 + APK 结构),失败抛异常由调用方按失败处理。
    /// </summary>
    public async Task<VivoRootManagerResource> EnsureManagerApkAsync(
        VivoRootManagerResource manager,
        CancellationToken cancellationToken)
    {
        if (File.Exists(manager.ApkPath))
        {
            try
            {
                VerifyManagerApk(manager);
                return manager;
            }
            catch (Exception exception) when (exception is InvalidDataException or FileNotFoundException)
            {
                // 本地文件损坏/为空:视为缺失,重新下载到缓存。
            }
        }

        if (downloader is null)
        {
            throw new InvalidOperationException($"{manager.Key} 管理器 APK 缺失且没有配置下载器。");
        }

        var cachePath = CacheApkPath(manager.Key);
        var spec = new RemoteAssetSpec(
            $"{manager.Key} 管理器 APK",
            RemoteAssetCatalog.GitHubDownloadUrl(ApkFileName(manager.Key)),
            ExpectedSha256: ManagerApkSha256.GetValueOrDefault(manager.Key));
        await downloader.DownloadAsync(spec, cachePath, progress: null, cancellationToken);

        var cached = manager with { ApkPath = cachePath };
        // 下载后的强校验:哈希不符 / 非有效 APK 都会抛。
        VerifyManagerApk(cached);
        return cached;
    }

    public VivoRootToolResource ResolveMagiskboot() => new(
        "magiskboot",
        Path.Combine(projectRoot, "root-tools", "magiskboot.so"));

    public void VerifyRootTool(VivoRootToolResource tool)
    {
        var file = new FileInfo(tool.Path);
        if (!file.Exists)
        {
            throw new FileNotFoundException($"未找到 {tool.Name} 工具。", tool.Path);
        }

        if (file.Length == 0)
        {
            throw new InvalidDataException($"{tool.Name} 工具为空。" );
        }
    }

    public static string ValidateKmi(string kmi)
    {
        if (!kSupportedKmis.Contains(kmi, StringComparer.Ordinal))
        {
            throw new ArgumentException($"不支持的 KMI: {kmi}", nameof(kmi));
        }

        return kmi;
    }

    public static string MapKernelRelease(string release)
    {
        var normalized = release.Trim();
        if (normalized.StartsWith("5.15.", StringComparison.Ordinal))
        {
            return "android13-5.15";
        }

        if (normalized.StartsWith("6.1.", StringComparison.Ordinal))
        {
            return "android14-6.1";
        }

        if (normalized.StartsWith("6.6.", StringComparison.Ordinal))
        {
            return "android15-6.6";
        }

        throw new ArgumentException($"无法映射 Vivo KernelSU KMI: {release}", nameof(release));
    }

    public void VerifyManagerApk(VivoRootManagerResource manager)
    {
        var file = new FileInfo(manager.ApkPath);
        if (!file.Exists)
        {
            throw new FileNotFoundException($"未找到 {manager.Key} 管理器 APK。", manager.ApkPath);
        }

        if (file.Length == 0)
        {
            throw new InvalidDataException($"{manager.Key} 管理器 APK 为空。");
        }

        // 完整性校验:比对 SHA-256 与随包分发的期望值,防止资源目录里的 APK
        // 被替换/篡改后仍被 root 权限安装并自动启动。哈希校验是最强且最简的信号。
        if (ManagerApkSha256.TryGetValue(manager.Key, out var expectedHash))
        {
            using var stream = File.OpenRead(manager.ApkPath);
            var actualHash = Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
            if (!string.Equals(actualHash, expectedHash, StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidDataException($"{manager.Key} 管理器 APK 完整性校验失败（SHA-256 不匹配）。");
            }
        }

        // 至少要是可读的 APK:含 AndroidManifest.xml 的 ZIP 结构。
        try
        {
            using var archive = ZipFile.OpenRead(manager.ApkPath);
            if (!archive.Entries.Any(entry => entry.FullName == "AndroidManifest.xml"))
            {
                throw new InvalidDataException($"{manager.Key} 管理器 APK 不是有效的 APK（缺少 AndroidManifest.xml）。");
            }
        }
        catch (InvalidDataException)
        {
            throw;
        }
        catch (Exception exception)
        {
            throw new InvalidDataException($"{manager.Key} 管理器 APK 不是有效的 APK。", exception);
        }
    }

    public string ExtractVerifiedLibKsud(VivoRootManagerResource manager, string abi, string destination)
    {
        if (!manager.Libraries.ContainsKey(abi))
        {
            throw new ArgumentException($"{manager.Key} 不支持设备 ABI: {abi}", nameof(abi));
        }

        VerifyManagerApk(manager);
        var entryName = $"lib/{abi}/libksud.so";
        var output = Path.GetFullPath(destination);
        var pending = output + ".pending";
        Directory.CreateDirectory(Path.GetDirectoryName(output)!);

        try
        {
            using var archive = ZipFile.OpenRead(manager.ApkPath);
            var entries = archive.Entries.Where(entry => entry.FullName == entryName).ToArray();
            if (entries.Length != 1)
            {
                throw new InvalidDataException($"APK 中必须存在唯一的 {entryName}。" );
            }

            var entry = entries[0];
            using (var input = entry.Open())
            using (var outputStream = File.Create(pending))
            {
                input.CopyTo(outputStream);
            }

            if (new FileInfo(pending).Length == 0)
            {
                throw new InvalidDataException("APK 中的 libksud.so 为空。" );
            }

            File.Move(pending, output, true);
            return output;
        }
        catch
        {
            File.Delete(pending);
            throw;
        }
    }

}
