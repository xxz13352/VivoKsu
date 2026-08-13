using System.IO;
using System.Text.Json;

namespace VivoKsu.App.Services;

public sealed class ToolPathPreferences
{
    private static readonly JsonSerializerOptions SerializerOptions = new() { WriteIndented = true };
    private readonly string settingsPath;
    private ToolPathSettings settings;

    public ToolPathPreferences(string settingsPath)
    {
        this.settingsPath = settingsPath;
        settings = Load(settingsPath);
    }

    public string? ScrcpyPath => settings.ScrcpyPath;

    public static ToolPathPreferences CreateDefault()
    {
        var directory = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "VivoKsu");
        return new ToolPathPreferences(Path.Combine(directory, "settings.json"));
    }

    public void SaveScrcpyPath(string toolPath)
    {
        settings = settings with { ScrcpyPath = Path.GetFullPath(toolPath) };
        Persist();
    }

    public void ClearScrcpyPath()
    {
        settings = settings with { ScrcpyPath = null };
        Persist();
    }

    private static ToolPathSettings Load(string path)
    {
        try
        {
            if (!File.Exists(path))
            {
                return ToolPathSettings.Empty;
            }

            return JsonSerializer.Deserialize<ToolPathSettings>(File.ReadAllText(path)) ?? ToolPathSettings.Empty;
        }
        catch (JsonException)
        {
            return ToolPathSettings.Empty;
        }
        catch (IOException)
        {
            return ToolPathSettings.Empty;
        }
    }

    private void Persist()
    {
        var directory = Path.GetDirectoryName(settingsPath);
        if (!string.IsNullOrWhiteSpace(directory))
        {
            Directory.CreateDirectory(directory);
        }

        var temporaryPath = $"{settingsPath}.tmp";
        File.WriteAllText(temporaryPath, JsonSerializer.Serialize(settings, SerializerOptions));
        File.Move(temporaryPath, settingsPath, true);
    }

    private sealed record ToolPathSettings(string? ScrcpyPath)
    {
        public static ToolPathSettings Empty { get; } = new(ScrcpyPath: null);
    }
}
