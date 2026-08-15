using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class DotNetRuntimeDetectorTests
{
    [Theory]
    [InlineData("8.0.30", true)]
    [InlineData("8.0.23", true)]
    [InlineData("8.0.0", true)]
    [InlineData("9.0.0", false)] // App 未开 RollForward,9/10 运行时无法承载 net8.0
    [InlineData("10.0.3", false)]
    [InlineData("7.0.0", false)]
    [InlineData("", false)]
    [InlineData("not-a-version", false)]
    public void IsDesktop8OrNewer_accepts_only_major_8(string? version, bool expected) =>
        Assert.Equal(expected, DotNetRuntimeDetector.IsDesktop8OrNewer(version));

    [Fact]
    public void ParseVersionFromListRuntimesLine_extracts_version()
    {
        Assert.Equal(
            "8.0.23",
            DotNetRuntimeDetector.ParseVersionFromListRuntimesLine(
                "Microsoft.WindowsDesktop.App 8.0.23 [C:\\Program Files\\dotnet\\shared\\Microsoft.WindowsDesktop.App]"));
        Assert.Null(
            DotNetRuntimeDetector.ParseVersionFromListRuntimesLine("Microsoft.NETCore.App 8.0.23 [x]"));
        Assert.Null(DotNetRuntimeDetector.ParseVersionFromListRuntimesLine("junk"));
    }

    [Fact]
    public void HasDesktopRuntime8_uses_registry_result_when_conclusive()
    {
        // 注册表给出结论时不再调用进程探测(若被调用则抛错暴露)。
        Assert.True(DotNetRuntimeDetector.HasDesktopRuntime8(
            registryProbe: () => true,
            processProbe: () => throw new InvalidOperationException("不应回退到进程探测。")));
        Assert.False(DotNetRuntimeDetector.HasDesktopRuntime8(
            registryProbe: () => false,
            processProbe: () => throw new InvalidOperationException("不应回退到进程探测。")));
    }

    [Fact]
    public void HasDesktopRuntime8_falls_back_to_process_when_registry_inconclusive()
    {
        Assert.True(DotNetRuntimeDetector.HasDesktopRuntime8(
            registryProbe: () => null,
            processProbe: () => true));
        Assert.False(DotNetRuntimeDetector.HasDesktopRuntime8(
            registryProbe: () => null,
            processProbe: () => false));
    }
}
