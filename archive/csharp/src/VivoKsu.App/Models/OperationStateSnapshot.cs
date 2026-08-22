namespace VivoKsu.App.Models;

public sealed record OperationStateSnapshot(
    OperationKind Kind,
    string? OperationId,
    string Title,
    string Stage,
    double? Progress,
    DateTimeOffset? StartedAt,
    bool IsCancellable)
{
    public static OperationStateSnapshot Idle { get; } = new(
        OperationKind.Idle,
        null,
        string.Empty,
        string.Empty,
        null,
        null,
        false);
}
