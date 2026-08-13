namespace VivoKsu.Server.Models;

/// <summary>一条 ROM 下载记录,服务端返回给客户端(桌面应用)。</summary>
public sealed record RomInfo(
    string Pd,
    string Version,
    string Url,
    string? Name = null,
    long? SizeBytes = null,
    string? Sha256 = null);
