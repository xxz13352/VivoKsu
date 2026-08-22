using System.Text.Json.Serialization;

namespace VivoKsu.App.Models;

/// <summary>
/// 一条客户端使用记录(由 OperationCoordinator 在每次用户操作完成时产生,批量上传到服务端分类存储)。
/// 服务端按 operation_kind 分类;归属用户由客户端 token 在服务端解析,客户端不上传用户标识。
/// JSON 字段刻意用 snake_case,与服务端 /api/usage/logs 契约及后台展示一致。
/// </summary>
public sealed record UsageLogEntry(
    [property: JsonPropertyName("operation")] string Operation,
    [property: JsonPropertyName("title")] string Title,
    [property: JsonPropertyName("status")] string Status,                    // success / failed / canceled
    [property: JsonPropertyName("event_id")] string EventId,                  // 每次操作唯一键,服务端幂等去重
    [property: JsonPropertyName("started_at")] long StartedAtEpochSeconds,
    [property: JsonPropertyName("ended_at")] long? EndedAtEpochSeconds,
    [property: JsonPropertyName("duration_ms")] long? DurationMs);
