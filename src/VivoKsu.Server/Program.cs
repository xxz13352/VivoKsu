using VivoKsu.Server.Models;
using VivoKsu.Server.Services;

var builder = WebApplication.CreateBuilder(args);

var votaOptions = VotaApiOptions.FromConfiguration(builder.Configuration);
builder.Services.AddSingleton(votaOptions);
builder.Services.AddHttpClient<VotaApiRomSource>();

// VOTA 凭据(账号 / API Token / device_id)都未配置时退回演示数据源,保证服务端开箱即可联调。
builder.Services.AddSingleton<IRomSource>(sp =>
    string.IsNullOrWhiteSpace(votaOptions.User)
    && string.IsNullOrWhiteSpace(votaOptions.ApiToken)
    && string.IsNullOrWhiteSpace(votaOptions.DeviceId)
        ? new DemoRomSource()
        : sp.GetRequiredService<VotaApiRomSource>());

var app = builder.Build();

// 生产/开发走 HTTPS 重定向;Testing 环境下测试客户端无 TLS 证书,跳过。
if (!app.Environment.IsEnvironment("Testing"))
{
    app.UseHttpsRedirection();
}

app.MapGet("/health", () => Results.Ok(new { status = "ok", source = SourceName(app.Services.GetRequiredService<IRomSource>()) }));

// 客户端(桌面应用)按 PD + 版本号查询 ROM 下载链接。
app.MapGet("/api/rom", async (string? pd, string? version, IRomSource source, CancellationToken cancellationToken) =>
{
    if (string.IsNullOrWhiteSpace(pd))
    {
        return Results.BadRequest(new { error = "缺少 pd 查询参数。" });
    }

    if (string.IsNullOrWhiteSpace(version))
    {
        return Results.BadRequest(new { error = "缺少 version 查询参数。" });
    }

    try
    {
        var rom = await source.ResolveAsync(pd, version, cancellationToken);
        return rom is null
            ? Results.NotFound(new { error = $"未找到 {pd} {version} 对应的 ROM。" })
            : Results.Ok(rom);
    }
    catch (RomResolveException exception)
    {
        return MapVotaError(exception.ErrorCode, exception.Message);
    }
    catch (HttpRequestException)
    {
        return Results.Json(new { error = "无法连接上游 ROM API。" }, statusCode: StatusCodes.Status502BadGateway);
    }
});

app.Run();

static string SourceName(IRomSource source) => source.GetType().Name;

static IResult MapVotaError(string? code, string message)
{
    // VOTA 某些失败只带 error 文本(如 "record not found"),无 code,按文案兜底映射。
    var inferredNotFound = code is null && message.Contains("not found", StringComparison.OrdinalIgnoreCase);
    if (inferredNotFound)
    {
        return Results.NotFound(new { error = message });
    }

    return code switch
    {
        "NOT_FOUND" => Results.NotFound(new { error = message }),
        "AUTH_FAIL" => Results.Json(new { error = message }, statusCode: StatusCodes.Status401Unauthorized),
        "FORBIDDEN" => Results.Json(new { error = message }, statusCode: StatusCodes.Status403Forbidden),
        "INSUFFICIENT_CREDITS" => Results.Json(new { error = message }, statusCode: StatusCodes.Status402PaymentRequired),
        "RATE_LIMITED" => Results.Json(new { error = message }, statusCode: StatusCodes.Status429TooManyRequests),
        _ => Results.Json(new { error = message }, statusCode: StatusCodes.Status502BadGateway),
    };
}

// 便于集成测试引用 Program 类型。
public partial class Program;
