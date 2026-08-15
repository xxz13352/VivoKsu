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
    [Fact]
    public void Write_persists_formatted_lines_to_the_log_file()
    {
        var logPath = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"), "operations.log");
        var service = new OperationLogService(logPath);
        try
        {
            service.Write(OperationLogLevel.Info, "开始刷写");
            service.Write(OperationLogLevel.Error, "vendor_boot 修补失败");

            var lines = File.ReadAllLines(logPath);
            Assert.Equal(2, lines.Length);
            Assert.Matches(@"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} \[Info\] 开始刷写$", lines[0]);
            Assert.Contains("[Error] vendor_boot 修补失败", lines[1]);
        }
        finally
        {
            TryDeleteDirectory(Path.GetDirectoryName(logPath)!);
        }
    }

    [Fact]
    public void Null_path_writes_only_to_memory()
    {
        var defaultPath = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "VivoKsu",
            "operations.log");
        var before = File.Exists(defaultPath);

        var service = new OperationLogService();
        service.Write(OperationLogLevel.Info, "仅内存");

        // 未指定路径时不落盘:默认文件状态不变。
        Assert.Equal(before, File.Exists(defaultPath));
    }

    [Fact]
    public void Clear_keeps_the_disk_log_for_troubleshooting()
    {
        var logPath = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"), "operations.log");
        var service = new OperationLogService(logPath);
        try
        {
            service.Write(OperationLogLevel.Info, "第一条");

            service.Clear();

            Assert.Empty(service.Entries);           // UI 面板清空
            Assert.True(File.Exists(logPath));       // 磁盘原始记录保留
            Assert.Contains("第一条", File.ReadAllText(logPath));
        }
        finally
        {
            TryDeleteDirectory(Path.GetDirectoryName(logPath)!);
        }
    }

    private static void TryDeleteDirectory(string path)
    {
        try
        {
            if (Directory.Exists(path))
            {
                Directory.Delete(path, true);
            }
        }
        catch
        {
            // Best effort.
        }
    }
}
