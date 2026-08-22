using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public interface IOperationCoordinator
{
    bool IsBusy { get; }

    OperationStateSnapshot State { get; }

    event EventHandler? StateChanged;

    Task RunAsync(
        OperationKind kind,
        string title,
        Func<OperationContext, CancellationToken, Task> operation,
        CancellationToken cancellationToken = default);

    void CancelCurrent();
}
