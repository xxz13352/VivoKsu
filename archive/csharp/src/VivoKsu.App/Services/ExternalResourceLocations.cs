using System.IO;

namespace VivoKsu.App.Services;

/// <summary>
/// 按需下载资源的本地固定位置(发布瘦身后 scrcpy / APK / payload_dumper 落盘于此)。
/// 固定根为 <see cref="PreferredRoot"/>(<c>C:\nwflash</c>,机器级、跨用户);写 C:\ 根
/// 需管理员权限,无权限时自动回退到用户目录 <c>%LOCALAPPDATA%\VivoKsu</c>。
/// 每次会话只解析一次;三个资源目录都基于 <see cref="Root"/> 计算,「下次检测文件
/// 存在则复用」的逻辑由各 provisioner 的 <c>File.Exists</c> 检查完成。
/// </summary>
public static class ExternalResourceLocations
{
    /// <summary>固定释放根目录。C:\ 根下创建/写入需管理员权限(应用默认非提权运行)。</summary>
    public const string PreferredRoot = @"C:\nwflash";

    private static readonly string effectiveRoot = ResolveRoot();

    /// <summary>本次会话生效的根目录(优先 C:\nwflash,不可写回退 %LOCALAPPDATA%\VivoKsu)。</summary>
    public static string Root => effectiveRoot;

    private static string ResolveRoot() =>
        TryMakeWritable(PreferredRoot)
            ? PreferredRoot
            : Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "VivoKsu");

    /// <summary>探测目录是否可创建且可写(建目录 + 写删探针文件)。不可写返回 false,调用方回退。</summary>
    internal static bool TryMakeWritable(string root)
    {
        try
        {
            Directory.CreateDirectory(root);
            var probe = Path.Combine(root, $".write-{Guid.NewGuid():N}.tmp");
            File.WriteAllText(probe, "ok");
            File.Delete(probe);
            return true;
        }
        catch (Exception exception) when (exception is UnauthorizedAccessException or IOException)
        {
            return false;
        }
    }
}
