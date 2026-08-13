using VivoKsu.Server.Models;

namespace VivoKsu.Server.Services;

/// <summary>
/// 演示用数据源:VOTA 凭据未配置时启用,返回一条占位链接,
/// 方便客户端先联调服务端接口,不会真实下载。
/// </summary>
public sealed class DemoRomSource : IRomSource
{
    public Task<RomInfo?> ResolveAsync(string pd, string version, CancellationToken cancellationToken)
    {
        var rom = new RomInfo(
            pd,
            version,
            $"https://example.invalid/roms/{Uri.EscapeDataString(pd)}/{Uri.EscapeDataString(version)}/full.zip",
            Name: $"{pd} demo ROM",
            SizeBytes: 1024L * 1024 * 1024);
        return Task.FromResult<RomInfo?>(rom);
    }
}
