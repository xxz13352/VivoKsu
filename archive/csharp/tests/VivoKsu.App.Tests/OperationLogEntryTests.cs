using VivoKsu.App.Models;

namespace VivoKsu.App.Tests;

public class OperationLogEntryTests
{
    [Theory]
    [InlineData(OperationLogLevel.Info, "信息")]
    [InlineData(OperationLogLevel.Success, "完成")]
    [InlineData(OperationLogLevel.Warning, "注意")]
    [InlineData(OperationLogLevel.Error, "失败")]
    public void DisplayLevel_uses_compact_chinese_labels(OperationLogLevel level, string expected)
    {
        var entry = new OperationLogEntry(DateTimeOffset.UnixEpoch, level, "test");
        var displayLevel = typeof(OperationLogEntry).GetProperty("DisplayLevel");

        Assert.NotNull(displayLevel);
        Assert.Equal(expected, displayLevel.GetValue(entry));
    }
}
