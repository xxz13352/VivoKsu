namespace VivoKsu.App.Models;

/// <summary>下载进度:已下载字节 / 总字节(未知为 null)/ 实时速度。</summary>
public sealed record DownloadProgress(long DownloadedBytes, long? TotalBytes, double BytesPerSecond);
