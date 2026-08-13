using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Tests;

public class OperationLogServiceTests
{
    [Fact]
    public void Write_keeps_the_newest_500_entries()
    {
        var service = new OperationLogService();

        for (var index = 0; index < 501; index++)
        {
            service.Write(OperationLogLevel.Info, $"line-{index}");
        }

        Assert.Equal(500, service.Entries.Count);
        Assert.Equal("line-1", service.Entries[0].Message);
        Assert.Equal("line-500", service.Entries[^1].Message);
    }

    [Fact]
    public void ClearCommand_removes_the_current_session_entries_and_updates_the_count()
    {
        var service = new OperationLogService();
        service.Write(OperationLogLevel.Info, "first");
        service.Write(OperationLogLevel.Success, "second");
        var viewModel = new OperationLogViewModel(service);

        var countText = viewModel.GetType().GetProperty("EntryCountText");
        var clearCommand = viewModel.GetType().GetProperty("ClearCommand");

        Assert.NotNull(countText);
        Assert.NotNull(clearCommand);
        Assert.Equal("2 条记录", countText!.GetValue(viewModel));

        ((System.Windows.Input.ICommand)clearCommand!.GetValue(viewModel)!).Execute(null);

        Assert.Empty(service.Entries);
        Assert.Equal("0 条记录", countText.GetValue(viewModel));
    }

    [Fact]
    public void HasEntries_tracks_writes_and_clears_for_the_log_empty_state()
    {
        var service = new OperationLogService();
        var viewModel = new OperationLogViewModel(service);

        Assert.False(viewModel.HasEntries);

        service.Write(OperationLogLevel.Info, "session started");

        Assert.True(viewModel.HasEntries);

        viewModel.ClearCommand.Execute(null);

        Assert.False(viewModel.HasEntries);
    }
}
