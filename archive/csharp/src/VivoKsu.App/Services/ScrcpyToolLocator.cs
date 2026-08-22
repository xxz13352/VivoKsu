using System.IO;

namespace VivoKsu.App.Services;

public interface IScrcpyToolLocator
{
    bool IsAvailable { get; }
    string? ExecutablePath { get; }
    string StatusMessage { get; }
    void ConfigureToolPath(string toolPath);
}

public sealed class ScrcpyToolLocator : IScrcpyToolLocator
{
    private readonly IReadOnlyList<string> searchPaths;
    private string? externalExecutablePath;

    public ScrcpyToolLocator(string applicationRoot, IEnumerable<string>? searchPaths = null)
    {
        ApplicationRoot = applicationRoot;
        this.searchPaths = searchPaths?.ToArray() ?? ReadPathEntries();
    }

    public string ApplicationRoot { get; }

    public string? ExecutablePath => externalExecutablePath ?? FindExecutable();

    public bool IsAvailable => ExecutablePath is not null;

    public string StatusMessage => IsAvailable
        ? externalExecutablePath is not null ? "scrcpy 已就绪（外部工具）" : "scrcpy 已就绪"
        : "未检测到 scrcpy.exe";

    public void ConfigureToolPath(string toolPath)
    {
        if (string.IsNullOrWhiteSpace(toolPath) || !string.Equals(Path.GetExtension(toolPath), ".exe", StringComparison.OrdinalIgnoreCase))
        {
            throw new ArgumentException("请选择有效的 scrcpy.exe 文件。", nameof(toolPath));
        }

        var fullPath = Path.GetFullPath(toolPath);
        if (!File.Exists(fullPath) || new FileInfo(fullPath).Length == 0)
        {
            throw new FileNotFoundException("选择的 scrcpy.exe 不存在或为空。", fullPath);
        }

        externalExecutablePath = fullPath;
    }

    private string? FindExecutable()
    {
        var bundledScrcpyPath = Path.Combine(ApplicationRoot, "scrcpy", "scrcpy.exe");
        if (File.Exists(bundledScrcpyPath))
        {
            return bundledScrcpyPath;
        }

        var bundledPath = Path.Combine(ApplicationRoot, "platform-tools", "scrcpy.exe");
        if (File.Exists(bundledPath))
        {
            return bundledPath;
        }

        foreach (var directory in searchPaths)
        {
            var candidate = Path.Combine(directory, "scrcpy.exe");
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        return null;
    }

    private static IReadOnlyList<string> ReadPathEntries()
    {
        var path = Environment.GetEnvironmentVariable("PATH") ?? string.Empty;
        return path.Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
    }
}
