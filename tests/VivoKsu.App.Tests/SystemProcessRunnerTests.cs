using System.Diagnostics;
using System.Reflection;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class SystemProcessRunnerTests
{
    [Fact]
    public void CreateStartInfo_uses_the_executable_directory_as_the_working_directory()
    {
        var createStartInfo = typeof(SystemProcessRunner).GetMethod(
            "CreateStartInfo",
            BindingFlags.Static | BindingFlags.NonPublic);

        Assert.NotNull(createStartInfo);
        var startInfo = (ProcessStartInfo)createStartInfo!.Invoke(null, new object[]
        {
            "C:\\tools\\scrcpy\\scrcpy.exe",
            (IReadOnlyList<string>)new[] { "--serial", "RF8" }
        })!;

        Assert.Equal("C:\\tools\\scrcpy", startInfo.WorkingDirectory);
        Assert.Equal("scrcpy.exe", Path.GetFileName(startInfo.FileName));
        Assert.Equal(new[] { "--serial", "RF8" }, startInfo.ArgumentList);
    }
}
