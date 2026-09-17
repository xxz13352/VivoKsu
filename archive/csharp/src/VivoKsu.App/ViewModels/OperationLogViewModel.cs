using System.Collections.ObjectModel;
using System.Collections.Specialized;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.ViewModels;

public sealed partial class OperationLogViewModel : ObservableObject
{
    private readonly OperationLogService operationLog;

    public OperationLogViewModel(OperationLogService operationLog)
    {
        this.operationLog = operationLog;
        Entries = operationLog.Entries;
        Entries.CollectionChanged += OnEntriesCollectionChanged;
        ClearCommand = new RelayCommand(operationLog.Clear, () => Entries.Count > 0);
    }

    public ObservableCollection<OperationLogEntry> Entries { get; }

    public IRelayCommand ClearCommand { get; }

    public string EntryCountText => $"{Entries.Count} 条记录";

    public int EntryCount => Entries.Count;

    public bool HasEntries => Entries.Count > 0;

    private void OnEntriesCollectionChanged(object? sender, NotifyCollectionChangedEventArgs args)
    {
        OnPropertyChanged(nameof(EntryCountText));
        OnPropertyChanged(nameof(EntryCount));
        OnPropertyChanged(nameof(HasEntries));
        ClearCommand.NotifyCanExecuteChanged();
    }
}
