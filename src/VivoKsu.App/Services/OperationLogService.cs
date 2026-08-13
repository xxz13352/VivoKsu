using System.Collections.ObjectModel;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class OperationLogService
{
    private const int MaximumEntries = 500;

    public ObservableCollection<OperationLogEntry> Entries { get; } = [];

    public void Write(OperationLogLevel level, string message, string? operationId = null)
    {
        Entries.Add(new OperationLogEntry(DateTimeOffset.Now, level, message, operationId));

        while (Entries.Count > MaximumEntries)
        {
            Entries.RemoveAt(0);
        }
    }

    public void Clear()
    {
        Entries.Clear();
    }
}
