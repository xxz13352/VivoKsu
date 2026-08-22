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

    public string? Username => settings.Username;

    public string? Token => settings.Token;

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

    /// <summary>保存登录凭据(记住登录)。</summary>
    public void SaveCredentials(string username, string token)
    {
        settings = settings with { Username = username, Token = token };
        Persist();
    }

    public void ClearCredentials()
    {
        settings = settings with { Username = null, Token = null };
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

    private sealed record ToolPathSettings(string? ScrcpyPath, string? Username, string? Token)
    {
        public static ToolPathSettings Empty { get; } = new(ScrcpyPath: null, Username: null, Token: null);
    }
}
