namespace VivoKsu.Server.Services;

/// <summary>
/// VOTA API(https://api.otau.cc.cd)的访问配置。凭据放在服务端,
/// 桌面应用只需用 PD + 版本号查询即可。
/// </summary>
public sealed class VotaApiOptions
{
    /// <summary>VOTA API 基地址。</summary>
    public string BaseUrl { get; set; } = "https://api.otau.cc.cd";

    /// <summary>VOTA 用户名(线刷/OTA 端点需要 user+pass)。</summary>
    public string User { get; set; } = string.Empty;

    /// <summary>VOTA 密码。</summary>
    public string Pass { get; set; } = string.Empty;

    /// <summary>
    /// API Token(第三方工具/自动化脚本接入)。通过 <c>Authorization: Bearer &lt;token&gt;</c> 鉴权,
    /// 不暴露密码;每个用户最多 10 个有效 token,明文仅创建时显示一次。配置后优先于 user/pass。
    /// </summary>
    public string ApiToken { get; set; } = string.Empty;

    /// <summary>客户端版本号(平台白名单,默认 0.1.0)。</summary>
    public string Ver { get; set; } = "0.1.0";

    /// <summary>
    /// 使用设备端端点时的设备标识(64 位十六进制 SHA256)。
    /// 配置后改用 dev_resolve,无需 user/pass。
    /// </summary>
    public string DeviceId { get; set; } = string.Empty;

    /// <summary>调用的 VOTA action:resolve_url(OTA 链接)/ resolve_flash_url(线刷包)/ dev_resolve。</summary>
    public string Action { get; set; } = "resolve_url";

    public bool UseDeviceAuth => !string.IsNullOrWhiteSpace(DeviceId);

    public bool UseApiToken => !string.IsNullOrWhiteSpace(ApiToken);

    public static VotaApiOptions FromConfiguration(IConfiguration configuration) =>
        configuration.GetSection("VotaApi").Get<VotaApiOptions>() ?? new VotaApiOptions();
}
