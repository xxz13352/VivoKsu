using System.ComponentModel;
using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class OperationLogViewModelTests
{
    [Fact]
    public void EntryCount_notifies_when_a_log_entry_is_written()
    {
        var service = new OperationLogService();
        var viewModel = new OperationLogViewModel(service);
        var changes = new List<string?>();
        viewModel.PropertyChanged += (_, args) => changes.Add(args.PropertyName);

        service.Write(OperationLogLevel.Info, "session started");
        var entryCount = typeof(OperationLogViewModel).GetProperty("EntryCount");

        Assert.NotNull(entryCount);
        Assert.Equal(1, entryCount.GetValue(viewModel));
        Assert.Contains("EntryCount", changes);
    }
}
