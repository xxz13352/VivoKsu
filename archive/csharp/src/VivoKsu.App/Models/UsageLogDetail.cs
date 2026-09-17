using System.Text.Json.Serialization;

namespace VivoKsu.App.Models;

/// <summary>
/// 一条操作过程明细(对齐 Rust <c>UsageLogDetail</c>):一次操作内部的阶段性日志,
/// 随 <see cref="UsageLogEntry"/> 的 <c>details</c> 一起上传,落到服务端 <c>details_json</c> 列。
/// JSON 字段用 snake_case,与服务端契约一致。
/// </summary>
/// <param name="TimestampEpochSeconds">明细产生时间(Unix 秒)。</param>
/// <param name="Level">日志级别(PascalCase:Info / Success / Warning / Error,与 Rust 一致)。</param>
/// <param name="Message">明细正文;服务端截断到 16384 字符并丢弃空消息。</param>
public sealed record UsageLogDetail(
    [property: JsonPropertyName("timestamp_utc")] long TimestampEpochSeconds,
    [property: JsonPropertyName("level")] string Level,
    [property: JsonPropertyName("message")] string Message);
