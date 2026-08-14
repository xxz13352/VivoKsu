using System.Diagnostics;
using System.Reflection;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class SystemProcessRunnerTests
{
    [Fact]
    public void CreateStartInfo_uses_the_executable_directory_as_the_working_directory()
    {
        var startInfo = InvokeCreateStartInfo(
            "C:\\tools\\scrcpy\\scrcpy.exe",
            new[] { "--serial", "RF8" },
            environment: null);

        Assert.Equal("C:\\tools\\scrcpy", startInfo.WorkingDirectory);
        Assert.Equal("scrcpy.exe", Path.GetFileName(startInfo.FileName));
        Assert.Equal(new[] { "--serial", "RF8" }, startInfo.ArgumentList);
    }

    [Fact]
    public void CreateStartInfo_applies_environment_variables_to_the_child_process()
    {
        var startInfo = InvokeCreateStartInfo(
            "C:\\tools\\scrcpy\\scrcpy.exe",
            new[] { "--serial", "RF8" },
            environment: new Dictionary<string, string> { ["ADB"] = "C:\\tools\\platform-tools\\adb.exe" });

        Assert.Equal("C:\\tools\\platform-tools\\adb.exe", startInfo.Environment["ADB"]);
    }

    private static ProcessStartInfo InvokeCreateStartInfo(
        string executable,
        IReadOnlyList<string> arguments,
        IReadOnlyDictionary<string, string>? environment)
    {
        var createStartInfo = typeof(SystemProcessRunner).GetMethod(
            "CreateStartInfo",
            BindingFlags.Static | BindingFlags.NonPublic);

        Assert.NotNull(createStartInfo);
        return (ProcessStartInfo)createStartInfo!.Invoke(null, new object?[]
        {
            executable,
            arguments,
            environment
        })!;
    }
}
