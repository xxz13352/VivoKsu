using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 完整性遥测上报器(接线 Rust 的 <c>integrity_reporter</c> 所覆盖的客户端场景):
/// 把 <c>POST /api/integrity/report</c> 挂到客户端可检测的完整性失败点上 ——
/// TLS 钉扎失败(<see cref="IntegrityReportPhase.PinValidation"/>)、心跳租约冲突/失效
/// (<see cref="IntegrityReportPhase.Heartbeat"/>)等。全部 best-effort:上报失败静默,
/// 绝不影响主流程(登录/心跳/退出)。
/// </summary>
public sealed class IntegrityReporter
{
    /// <summary>窗口限流为 60s/20 条(服务端);客户端再做一层防抖,避免心跳失败风暴刷满配额。</summary>
    private static readonly TimeSpan MinimumGap = TimeSpan.FromSeconds(10);

    private readonly OtaApiClient client;
    private readonly object gate = new();
    private long lastReportEpochSeconds;

    public IntegrityReporter(OtaApiClient client)
    {
        this.client = client;
    }

    /// <summary>
    /// 上报一条完整性事件(best-effort;本地限频 + 服务端 60s/20 条窗口限流 + event_id 幂等)。
    /// 未登录时匿名上报(服务端允许)。
    /// </summary>
    public void Report(IntegrityReportPhase phase, IntegrityReportReason reason)
    {
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        lock (gate)
        {
            if (now - lastReportEpochSeconds < MinimumGap.TotalSeconds)
            {
                return;
            }

            lastReportEpochSeconds = now;
        }

        // event_id:标识字符集内(服务端幂等键),同秒同因重复上报会被去重。
        var request = new IntegrityReportRequest
        {
            EventId = $"integrity-{now}-{(int)phase}-{(int)reason}",
            Phase = phase,
            Reason = reason,
            ClientVersion = AppInfo.Version,
            BuildId = ClientSession.BuildId,
            OccurredAtEpochSeconds = now,
        };

        _ = Task.Run(async () =>
        {
            try
            {
                await client.ReportIntegrityAsync(request, CancellationToken.None).ConfigureAwait(false);
            }
            catch
            {
                // 上报失败(离线/限流/服务端故障)静默:完整性遥测永远不能反向影响业务。
            }
        });
    }
}
