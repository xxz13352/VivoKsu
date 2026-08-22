using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class OperationContext
{
    private readonly Action<string?, double?, OperationKind?> report;

    internal OperationContext(string operationId, Action<string?, double?, OperationKind?> report)
    {
        OperationId = operationId;
        this.report = report;
    }

    public string OperationId { get; }

    public void ReportStage(string stage, OperationKind? kind = null)
    {
        if (string.IsNullOrWhiteSpace(stage))
        {
            throw new ArgumentException("操作阶段不能为空。", nameof(stage));
        }

        report(stage.Trim(), null, kind);
    }

    public void ReportProgress(double progress)
    {
        if (progress is < 0 or > 1)
        {
            throw new ArgumentOutOfRangeException(nameof(progress), progress, "操作进度必须在 0 到 1 之间。 ");
        }

        report(null, progress, null);
    }

    public void ThrowIfCancellationRequested(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
    }
}
