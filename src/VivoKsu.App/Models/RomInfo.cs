namespace VivoKsu.App.Models;

/// <summary>服务端返回的 ROM 下载记录,与服务端 VivoKsu.Server.Models.RomInfo 字段一致。</summary>
public sealed record RomInfo(
    string Pd,
    string Version,
    string Url,
    string? Name = null,
    long? SizeBytes = null,
    string? Sha256 = null);
