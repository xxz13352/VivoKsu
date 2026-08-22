using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class ScrcpyToolLocatorTests
{
    [Fact]
    public void Uses_the_bundled_scrcpy_directory_when_present()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var bundledDirectory = Path.Combine(root, "scrcpy");
        var executable = Path.Combine(bundledDirectory, "scrcpy.exe");
        Directory.CreateDirectory(bundledDirectory);
        File.WriteAllText(executable, "tool");

        try
        {
            var locator = new ScrcpyToolLocator(root, []);

            Assert.True(locator.IsAvailable);
            Assert.Equal(executable, locator.ExecutablePath);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void Uses_the_vendored_scrcpy_executable_when_present()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var tools = Path.Combine(root, "platform-tools");
        Directory.CreateDirectory(tools);
        var executable = Path.Combine(tools, "scrcpy.exe");
        File.WriteAllText(executable, "tool");

        try
        {
            var locator = new ScrcpyToolLocator(root, []);

            Assert.True(locator.IsAvailable);
            Assert.Equal(executable, locator.ExecutablePath);
            Assert.Equal("scrcpy 已就绪", locator.StatusMessage);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }

    [Fact]
    public void Reports_missing_when_neither_the_package_nor_path_contains_scrcpy()
    {
        var locator = new ScrcpyToolLocator(Path.Combine(Path.GetTempPath(), Guid.NewGuid().ToString("N")), []);

        Assert.False(locator.IsAvailable);
        Assert.Null(locator.ExecutablePath);
        Assert.Equal("未检测到 scrcpy.exe", locator.StatusMessage);
    }

    [Fact]
    public void ConfigureToolPath_enables_an_external_scrcpy_executable()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var executable = Path.Combine(root, "scrcpy.exe");
        Directory.CreateDirectory(root);
        File.WriteAllText(executable, "tool");
        var locator = new ScrcpyToolLocator(root, []);

        try
        {
            var configure = typeof(ScrcpyToolLocator).GetMethod("ConfigureToolPath");
            Assert.NotNull(configure);
            configure!.Invoke(locator, [executable]);

            Assert.True(locator.IsAvailable);
            Assert.Equal(executable, locator.ExecutablePath);
            Assert.Equal("scrcpy 已就绪（外部工具）", locator.StatusMessage);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }
}
