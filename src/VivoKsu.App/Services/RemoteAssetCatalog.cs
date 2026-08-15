namespace VivoKsu.App.Services;

/// <summary>
/// 外置资源的远程托管配置(发布瘦身:scrcpy / APK / payload_dumper 不再随包,首次使用时按需下载)。
/// 资产托管在 GitHub 公开仓库 Release;国内直连不稳,客户端按 <see cref="Mirrors"/> 顺序逐个 failover。
/// </summary>
/// <remarks>
/// 替换占位仓库:
///  1. 建公开仓库(如 <c>gh repo create vivoksu/nwflash-resources --public</c>);
///  2. 用 <c>scripts/Upload-Resources.ps1</c> 上传资产并创建 Release <see cref="ReleaseTag"/>;
///  3. 若 owner/repo/tag 与下面三个常量不符,改它们。
/// 镜像失效/换域名时更新 <see cref="Mirrors"/>(2026-08-15 逐个 HEAD 实测可用)。
/// </remarks>
public static class RemoteAssetCatalog
{
    public const string Owner = "vivoksu";
    public const string RepositoryName = "nwflash-resources";
    public const string ReleaseTag = "v1.0.0";

    /// <summary>构造 GitHub Release 资产的浏览器直链(下载时依次尝试直连与各镜像)。</summary>
    public static string GitHubDownloadUrl(string assetName) =>
        $"https://github.com/{Owner}/{RepositoryName}/releases/download/{ReleaseTag}/{assetName}";

    /// <summary>ghproxy 类镜像,请求形如 <c>{mirror}/{原始 github URL}</c>。全失败时下载器给手动链接。</summary>
    public static IReadOnlyList<string> Mirrors { get; } =
    [
        "https://gh-proxy.com/",
        "https://ghfast.top/",
        "https://ghproxy.net/"
    ];

    public const string PayloadDumperAssetName = "payload_dumper-win-x64.zip";
    public const string PayloadDumperExecutableName = "payload_dumper.exe";

    /// <summary>payload_dumper.exe 的 SHA-256(解压后校验;防下载源被替换成恶意二进制)。
    /// 重新生成:<c>certutil -hashfile payload_dumper.exe SHA256</c>。</summary>
    public const string PayloadDumperSha256 =
        "031b404609e804cd620fb10efdfce577b633f8b0ad8029fbd7170be3bc4cbe82";
}
