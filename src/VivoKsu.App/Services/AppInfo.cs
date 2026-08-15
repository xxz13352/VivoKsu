using System.Reflection;

namespace VivoKsu.App.Services;

/// <summary>
/// 当前桌面端版本信息。版本号来自 csproj 的 <c>&lt;Version&gt;</c>;
/// 发版前更新 <c>VivoKsu.App.csproj</c> 里的值即可。
/// </summary>
public static class AppInfo
{
    /// <summary>客户端版本号,如 "1.0.1"。用于 <c>X-Nwflash-Version</c> 头与版本校验。</summary>
    public static string Version { get; } =
        Assembly.GetExecutingAssembly().GetName().Version?.ToString(3) ?? "0.0.0";
}
