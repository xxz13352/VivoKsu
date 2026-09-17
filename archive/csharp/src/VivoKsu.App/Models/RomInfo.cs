namespace VivoKsu.App.Models;

/// <summary>api.nwflash.cc.cd 返回的 ROM 下载记录(与 Worker 响应字段一致)。</summary>
public sealed record RomInfo(
    string Pd,
    string Version,
    string Url,
    string? Name = null,
    long? SizeBytes = null,
    string? Sha256 = null);
