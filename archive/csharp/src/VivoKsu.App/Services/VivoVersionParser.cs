namespace VivoKsu.App.Services;

/// <summary>
/// vivo 设备版本号解析(从 SafeFlashViewModel 抽出,ROOT 云端提取与安全刷写共用):
/// 权威来源 <c>ro.build.version.bbk</c>,形如 "DPD2221B_A_15.2.12.0.W10.V000L1" ——
/// 第一段是设备代号,最后一段是完整版本号。
/// </summary>
public static class VivoVersionParser
{
    /// <summary>bbk 权威版本之外的候选属性(顺序即优先级)。</summary>
    public static readonly IReadOnlyList<string> VersionCandidates =
    [
        "ro.build.version.incremental",
        "ro.build.display.id",
        "ro.vivo.os.build.display.id",
    ];

    public static (string Codename, string Version) ParseBbkVersion(string value)
    {
        var parts = value.Split('_', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0)
        {
            return (string.Empty, string.Empty);
        }

        if (parts.Length == 1)
        {
            return (parts[0], parts[0]);
        }

        return (parts[0], parts[^1]);
    }

    public static bool IsGenericVersion(string value) =>
        value.Contains("release-keys", StringComparison.OrdinalIgnoreCase) ||
        value.Equals("unknown", StringComparison.OrdinalIgnoreCase) ||
        value.Equals("not found", StringComparison.OrdinalIgnoreCase);
}
