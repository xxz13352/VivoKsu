using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class ExternalResourceLocationsTests
{
    [Fact]
    public void PreferredRoot_is_the_fixed_C_drive_folder() =>
        Assert.Equal(@"C:\nwflash", ExternalResourceLocations.PreferredRoot);

    [Fact]
    public void Root_resolves_to_an_existing_directory()
    {
        // 无论本机 C:\nwflash 是否可写(管理员权限),Root 都应落到存在的目录:
        // 可写用固定根,否则回退 %LOCALAPPDATA%\VivoKsu(探测会创建它)。
        var root = ExternalResourceLocations.Root;
        Assert.False(string.IsNullOrWhiteSpace(root));
        Assert.True(Directory.Exists(root), $"Root 目录应存在: {root}");
    }

    [Fact]
    public void TryMakeWritable_detects_a_writable_directory()
    {
        var temp = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        try
        {
            Assert.True(ExternalResourceLocations.TryMakeWritable(temp));
            Assert.True(Directory.Exists(temp));
        }
        finally
        {
            TryDeleteDirectory(temp);
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
