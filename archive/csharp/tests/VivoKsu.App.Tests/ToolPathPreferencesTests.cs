using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class ToolPathPreferencesTests
{
    [Fact]
    public void Save_then_reload_preserves_scrcpy_path()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var settingsPath = Path.Combine(root, "settings.json");

        try
        {
            var preferences = new ToolPathPreferences(settingsPath);
            preferences.SaveScrcpyPath(@"D:\tools\scrcpy.exe");

            var restored = new ToolPathPreferences(settingsPath);

            Assert.Equal(@"D:\tools\scrcpy.exe", restored.ScrcpyPath);
        }
        finally
        {
            if (Directory.Exists(root))
            {
                Directory.Delete(root, true);
            }
        }
    }

    [Fact]
    public void Invalid_settings_file_falls_back_to_empty_preferences()
    {
        var root = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        var settingsPath = Path.Combine(root, "settings.json");
        Directory.CreateDirectory(root);
        File.WriteAllText(settingsPath, "not json");

        try
        {
            var preferences = new ToolPathPreferences(settingsPath);

            Assert.Null(preferences.ScrcpyPath);
        }
        finally
        {
            Directory.Delete(root, true);
        }
    }
}
