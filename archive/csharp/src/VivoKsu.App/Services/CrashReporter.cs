using System.IO;
using System.Text;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 崩溃报告补传(对齐 Rust <c>crash_uploader</c>):未捕获异常追加到
/// <c>%LOCALAPPDATA%\VivoKsu\crash.log</c>(每行 <c>[epoch] panic: ...</c>),下次启动时
/// 延迟读取 → 价值过滤 → 上传 <c>POST /api/diagnostics/crash</c> → 成功后清空。
/// 匿名可报(启动时通常尚未登录);上传失败保留原文件,下次启动重试;绝不阻塞启动。
/// </summary>
public sealed class CrashReporter
{
    /// <summary>启动后延迟多久再读 crash.log:避免与登录/版本门禁的启动关键路径竞争。</summary>
    public static readonly TimeSpan StartupDelay = TimeSpan.FromSeconds(8);

    /// <summary>crash.log 单文件读取上限;超过即只取尾部(最近的 panic 更有价值)。</summary>
    public const long MaxCrashLogBytes = 128 * 1024;

    private static readonly string[] HighRiskMarkers =
    [
        "-----BEGIN",                  // 私钥/证书块:整条拒绝(fail-closed,宁可不上传)
        "Authorization:",              // 认证头
        "Bearer ",
        "session_signing",             // 签名密钥环境变量名
        "PRIVATE KEY",
    ];

    private readonly string crashLogPath;

    public CrashReporter(string? crashLogPath = null)
    {
        this.crashLogPath = crashLogPath ?? DefaultCrashLogPath();
    }

    /// <summary>默认 crash.log 位置(与操作日志/使用日志同目录)。</summary>
    public static string DefaultCrashLogPath() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "VivoKsu",
        "crash.log");

    /// <summary>
    /// 记录一条未捕获异常(线程安全;任何写失败静默忽略)。
    /// 行格式对齐 Rust:<c>[epoch] panic: &lt;首行摘要&gt;</c>。
    /// </summary>
    public void Write(Exception? exception)
    {
        if (exception is null)
        {
            return;
        }

        try
        {
            var directory = Path.GetDirectoryName(crashLogPath);
            if (!string.IsNullOrEmpty(directory))
            {
                Directory.CreateDirectory(directory);
            }

            lock (this)
            {
                File.AppendAllText(
                    crashLogPath,
                    $"[{DateTimeOffset.UtcNow.ToUnixTimeSeconds()}] panic: {Sanitize(exception.ToString())}{Environment.NewLine}",
                    Encoding.UTF8);
            }
        }
        catch
        {
            // 日志写失败忽略。
        }
    }

    /// <summary>
    /// 上传上次崩溃(启动后台调用;对齐 Rust <c>run_pending_crash_upload</c>)。
    /// 返回是否上传成功(成功后 crash.log 已清空;失败保留,下次启动重试)。
    /// </summary>
    /// <param name="client">API 客户端(未登录时匿名上传,登录态带 token)。</param>
    /// <param name="sessionId">崩溃发生时无法得知的会话 id 用本次启动的占位(服务端仅校验字符集)。</param>
    public async Task<bool> UploadPendingAsync(OtaApiClient client, string sessionId, CancellationToken cancellationToken)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(sessionId);

        var contents = ReadBounded();
        if (string.IsNullOrEmpty(contents))
        {
            return false;
        }

        var report = BuildReport(Parse(contents), sessionId);
        if (report is null)
        {
            // 没有可上报的条目(全部被过滤/格式损坏):清空,避免每次启动都重读。
            Clear();
            return false;
        }

        try
        {
            await client.UploadCrashReportAsync(report, cancellationToken).ConfigureAwait(false);
        }
        catch
        {
            // 匿名上报也可能被 4xx 拒(限流/格式):保留 crash.log 下次重试,绝不影响启动。
            return false;
        }

        Clear();
        return true;
    }

    /// <summary>解析 crash.log(每行 <c>[epoch] panic: ...</c>);旧版时间戳格式或损坏行忽略。</summary>
    public static IReadOnlyList<PendingCrashEntry> Parse(string contents) =>
        contents
            .Split('\n')
            .Select(line => line.TrimEnd('\r'))
            .Select(line =>
            {
                var separator = line.IndexOf("] ", StringComparison.Ordinal);
                if (separator <= 1
                    || !long.TryParse(line[1..separator], out var epoch)
                    || !line[(separator + 2)..].StartsWith("panic: ", StringComparison.Ordinal))
                {
                    return null;
                }

                return new PendingCrashEntry(epoch, line[(separator + 9)..]);
            })
            .Where(entry => entry is not null)
            .Select(entry => entry!)
            .ToList();

    /// <summary>
    /// 构造补传请求:最新一条 panic 作为 <c>panic_message</c>,更早的条目拼入 <c>backtrace</c>,
    /// event_id 对齐 Rust(<c>crash-{epoch}-{len}</c>,清洗为标识字符集);
    /// 高危内容(私钥块等)的条目整条丢弃 —— 宁可不上传也不送原文。
    /// </summary>
    public CrashReportRequest? BuildReport(IReadOnlyList<PendingCrashEntry> entries, string sessionId)
    {
        var usable = entries
            .Where(entry => entry.OccurredAtEpochSeconds is >= 1 and <= 9_007_199_254_740_991)
            .Select(entry => new PendingCrashEntry(
                entry.OccurredAtEpochSeconds,
                Sanitize(entry.PanicText) ?? string.Empty))
            .Where(entry => entry.PanicText.Length > 0)
            .ToList();
        var newest = usable.LastOrDefault();
        if (newest is null)
        {
            return null;
        }

        // event_id 只允许标识字符,用受控清洗(对齐 Rust sanitize_identifier)。
        var raw = $"crash-{newest.OccurredAtEpochSeconds}-{newest.PanicText.Length}";
        var eventId = new string(raw
            .Take(64)
            .Select(ch => char.IsAsciiLetterOrDigit(ch) || ch is '.' or '_' or ':' or '-' ? ch : '-')
            .ToArray());

        var backtrace = string.Concat(usable.Take(usable.Count - 1)
            .Select(entry => entry.PanicText + "\n"));

        return new CrashReportRequest
        {
            EventId = eventId,
            ClientVersion = AppInfo.Version,
            BuildId = ClientSession.BuildId,
            SessionId = sessionId,
            PanicMessage = TruncateUtf8(newest.PanicText, CrashReportRequest.MaxPanicMessageBytes),
            Backtrace = TruncateUtf8(backtrace, CrashReportRequest.MaxBacktraceBytes),
            OccurredAtEpochSeconds = newest.OccurredAtEpochSeconds,
        };
    }

    /// <summary>
    /// 价值过滤(对齐 Rust <c>redact_crash_text</c> 的语义):路径/异常类型等有诊断价值的保留;
    /// 私钥块、认证头等高危内容 → 整条文本丢弃(不上传);已知凭据键值 → 替换为占位。
    /// </summary>
    public static string? Sanitize(string text)
    {
        if (string.IsNullOrEmpty(text))
        {
            return string.Empty;
        }

        // 私钥/证书块:fail-closed,整条不上传。
        foreach (var marker in HighRiskMarkers)
        {
            if (text.Contains(marker, StringComparison.OrdinalIgnoreCase))
            {
                return null;
            }
        }

        // 凭据键值对(token=xxx / password=xxx):替换为占位,保留结构便于排障。
        return System.Text.RegularExpressions.Regex.Replace(
            text,
            "(?i)(token|password|secret|api[_-]?key)\\s*[=:]\\s*\\S+",
            "$1=[CREDENTIAL_REMOVED]");
    }

    private static string TruncateUtf8(string value, int maxBytes)
    {
        var bytes = Encoding.UTF8.GetByteCount(value);
        if (bytes <= maxBytes)
        {
            return value;
        }

        var encoded = Encoding.UTF8.GetBytes(value);
        // 避免把多字节字符截成半个:回退到最近的字符边界。
        var end = maxBytes;
        while (end > 0 && (encoded[end] & 0xC0) == 0x80)
        {
            end--;
        }

        return Encoding.UTF8.GetString(encoded, 0, end);
    }

    private string ReadBounded()
    {
        try
        {
            if (!File.Exists(crashLogPath))
            {
                return string.Empty;
            }

            using var stream = File.OpenRead(crashLogPath);
            if (stream.Length > MaxCrashLogBytes)
            {
                stream.Seek(stream.Length - MaxCrashLogBytes, SeekOrigin.Begin);
            }

            using var reader = new StreamReader(stream, Encoding.UTF8);
            return reader.ReadToEnd();
        }
        catch
        {
            return string.Empty;
        }
    }

    private void Clear()
    {
        try
        {
            // 清空而非删除:崩溃钩子以追加模式持续写同一路径(对齐 Rust)。
            File.WriteAllText(crashLogPath, string.Empty, Encoding.UTF8);
        }
        catch
        {
            // 清空失败:下次启动可能重复上传,但服务端按 event_id 幂等去重。
        }
    }
}

/// <summary>上次崩溃记录的内存形态(解析后、过滤前;与 Rust <c>PendingCrashEntry</c> 同构)。</summary>
public sealed record PendingCrashEntry(long OccurredAtEpochSeconds, string PanicText);
