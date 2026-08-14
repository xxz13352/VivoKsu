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

    public VivoRootResourceService(string projectRoot)
    {
        this.projectRoot = Path.GetFullPath(projectRoot);
    }

    public static IReadOnlyList<string> SupportedKmis => kSupportedKmis;

    public IReadOnlyList<string> ManagerKeys => ManagerCatalog.Keys.ToArray();

    public VivoRootManagerResource ResolveManager(string key)
    {
        if (!ManagerCatalog.TryGetValue(key, out var catalog))
        {
            throw new ArgumentException($"不支持的 ROOT 管理器: {key}", nameof(key));
        }

        var fileName = catalog.Key == "KSU" ? "KSU.APK" : "KernelSU.apk";
        return catalog with { ApkPath = Path.Combine(projectRoot, "apk", fileName) };
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
