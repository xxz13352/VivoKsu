using VivoKsu.Server.Models;

namespace VivoKsu.Server.Services;

/// <summary>按设备 PD 码与版本号解析出对应的 ROM 下载记录。</summary>
public interface IRomSource
{
    Task<RomInfo?> ResolveAsync(string pd, string version, CancellationToken cancellationToken);
}
