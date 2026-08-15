using System.IO;
using System.Reflection;

namespace VivoKsu.App.Services;

/// <summary>把嵌入程序集的 wipe-data 镜像解到磁盘文件,供刷写 misc 分区用。异常消息保持模糊。</summary>
internal static class EmbeddedWipeData
{
    /// <summary>把内嵌 wipe-data 镜像写入 destinationPath,返回写入字节数。</summary>
    public static async Task<long> WriteToAsync(string destinationPath, CancellationToken cancellationToken)
    {
        var assembly = typeof(EmbeddedWipeData).Assembly;
        var resourceName = assembly.GetManifestResourceNames()
            .FirstOrDefault(name => name.EndsWith("wipe-data.img", StringComparison.OrdinalIgnoreCase))
            ?? throw new InvalidOperationException("数据清除资源不可用。");
        await using var input = assembly.GetManifestResourceStream(resourceName)
            ?? throw new InvalidOperationException("数据清除资源不可用。");
        await using (var output = new FileStream(destinationPath, FileMode.Create, FileAccess.Write, FileShare.None, 1 << 20, useAsync: true))
        {
            await input.CopyToAsync(output, cancellationToken);
        }

        return new FileInfo(destinationPath).Length;
    }
}
