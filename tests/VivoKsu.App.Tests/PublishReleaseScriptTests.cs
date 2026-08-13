using System.Diagnostics;

namespace VivoKsu.App.Tests;

public class PublishReleaseScriptTests
{
    [Fact]
    public async Task PublishReleaseScript_parses_under_windows_powershell()
    {
        var scriptPath = FindRepositoryFile("scripts", "Publish-Release.ps1");
        var escapedPath = scriptPath.Replace("'", "''", StringComparison.Ordinal);
        var parseCommand = $$"""
            $tokens = $null
            $errors = $null
            [System.Management.Automation.Language.Parser]::ParseFile('{{escapedPath}}', [ref]$tokens, [ref]$errors) | Out-Null
            $errors | ForEach-Object { $_.Message }
            if (@($errors).Count -gt 0) { exit 1 }
            """;
        var startInfo = new ProcessStartInfo
        {
            FileName = "powershell.exe",
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true
        };
        startInfo.ArgumentList.Add("-NoProfile");
        startInfo.ArgumentList.Add("-Command");
        startInfo.ArgumentList.Add(parseCommand);

        using var process = Process.Start(startInfo);
        Assert.NotNull(process);
        var standardOutput = await process!.StandardOutput.ReadToEndAsync();
        var standardError = await process.StandardError.ReadToEndAsync();
        await process.WaitForExitAsync();

        Assert.True(process.ExitCode == 0, $"Windows PowerShell 解析失败: {standardOutput}{standardError}");
    }

    private static string FindRepositoryFile(params string[] segments)
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            var candidate = Path.Combine([directory.FullName, .. segments]);
            if (File.Exists(candidate))
            {
                return candidate;
            }
        }

        throw new FileNotFoundException("未找到发布脚本。", Path.Combine(segments));
    }
}
