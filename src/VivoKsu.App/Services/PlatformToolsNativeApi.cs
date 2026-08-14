using System.Diagnostics;
using System.IO;
using System.Text;

namespace VivoKsu.App.Services;

public interface IPlatformToolsCommandRunner
{
    PlatformToolsCommandResult Run(string executable, IReadOnlyList<string> arguments, int timeoutMilliseconds = 15000);
}

public sealed record PlatformToolsCommandResult(int ExitCode, string StandardOutput, string StandardError)
{
    public string CombinedOutput => string.Join(
        Environment.NewLine,
        new[] { StandardOutput, StandardError }.Where(static text => !string.IsNullOrWhiteSpace(text)));
}

public sealed class SystemPlatformToolsCommandRunner : IPlatformToolsCommandRunner
{
    public PlatformToolsCommandResult Run(string executable, IReadOnlyList<string> arguments, int timeoutMilliseconds = 15000)
    {
        var startInfo = new ProcessStartInfo
        {
            FileName = executable,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
            StandardErrorEncoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false)
        };

        foreach (var argument in arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        using var process = Process.Start(startInfo)
            ?? throw new InvalidOperationException($"无法启动 {Path.GetFileName(executable)}。");
        var standardOutput = process.StandardOutput.ReadToEndAsync();
        var standardError = process.StandardError.ReadToEndAsync();
        if (!process.WaitForExit(timeoutMilliseconds))
        {
            KillProcessTree(process);
            throw new TimeoutException($"{Path.GetFileName(executable)} 执行超时（{timeoutMilliseconds / 1000} 秒），进程已终止。");
        }

        Task.WaitAll(standardOutput, standardError);
        return new PlatformToolsCommandResult(process.ExitCode, standardOutput.Result, standardError.Result);
    }

    private static void KillProcessTree(Process process)
    {
        try
        {
            process.Kill(entireProcessTree: true);
            process.WaitForExit();
        }
        catch
        {
            // Process already exited or could not be terminated; cleanup is best effort.
        }
    }
}

public sealed class PlatformToolsNativeApi : IFastbootRsNativeApi
{
    /// <summary>大分区刷写/读取可能远超默认 15s;平台工具回退路径必须给足超时,否则会在传输中途被强杀。</summary>
    private const int LongTransferTimeoutMilliseconds = 1_800_000; // 30 分钟

    private readonly IPlatformToolsCommandRunner commandRunner;
    private readonly string adbExecutable;
    private readonly string fastbootExecutable;

    public PlatformToolsNativeApi(
        IPlatformToolsCommandRunner commandRunner,
        string adbExecutable,
        string fastbootExecutable)
    {
        this.commandRunner = commandRunner;
        this.adbExecutable = adbExecutable;
        this.fastbootExecutable = fastbootExecutable;
    }

    public string ListDevices()
    {
        var adb = Run(adbExecutable, ["devices", "-l"]);
        var fastboot = Run(fastbootExecutable, ["devices"]);
        return string.Join("\n", ParseAdbDevices(adb.StandardOutput).Concat(ParseFastbootDevices(fastboot.StandardOutput)));
    }

    public string Shell(string? serial, string command) =>
        Run(adbExecutable, WithSerial(serial, "shell", command)).CombinedOutput;

    public string GetVar(string? serial, string variable)
    {
        var output = Run(fastbootExecutable, WithSerial(serial, "getvar", variable)).CombinedOutput;
        return ExtractGetVarValue(output, variable);
    }

    public void Reboot(string? serial, string target)
    {
        var arguments = new List<string>(WithSerial(serial, "reboot"));
        if (!string.IsNullOrWhiteSpace(target))
        {
            arguments.Add(target);
        }

        Run(adbExecutable, arguments);
    }

    /// <summary><c>fastboot reboot [&lt;target&gt;]</c> —— fastboot/fastbootd 模式下按目标重启(target 为空=回系统)。</summary>
    public void FastbootReboot(string? serial, string? target)
    {
        var arguments = new List<string>(WithSerial(serial, "reboot"));
        if (!string.IsNullOrWhiteSpace(target))
        {
            arguments.Add(target);
        }

        Run(fastbootExecutable, arguments);
    }

    public void SetActive(string? serial, string slot) =>
        Run(fastbootExecutable, WithSerial(serial, "set_active", slot));

    public void Push(string? serial, string localPath, string remotePath) =>
        Run(adbExecutable, WithSerial(serial, "push", localPath, remotePath));

    public long Pull(string? serial, string remotePath, string localPath)
    {
        Run(adbExecutable, WithSerial(serial, "pull", remotePath, localPath));
        return File.Exists(localPath) ? new FileInfo(localPath).Length : 0;
    }

    public string Install(string? serial, string apkPath, bool replace)
    {
        var arguments = new List<string>(WithSerial(serial, "install"));
        if (replace)
        {
            arguments.Add("-r");
        }

        arguments.Add(apkPath);
        return Run(adbExecutable, arguments).CombinedOutput;
    }

    public void Flash(string? serial, string partition, string imagePath) =>
        Run(fastbootExecutable, WithSerial(serial, "flash", partition, imagePath), LongTransferTimeoutMilliseconds);

    public void Erase(string? serial, string partition) =>
        Run(fastbootExecutable, WithSerial(serial, "erase", partition));

    public long Fetch(string? serial, string partition, string outputPath)
    {
        Run(fastbootExecutable, WithSerial(serial, "fetch", partition, outputPath), LongTransferTimeoutMilliseconds);
        return File.Exists(outputPath) ? new FileInfo(outputPath).Length : 0;
    }

    private PlatformToolsCommandResult Run(string executable, IReadOnlyList<string> arguments, int timeoutMilliseconds = 15000)
    {
        var result = commandRunner.Run(executable, arguments, timeoutMilliseconds);
        if (result.ExitCode != 0)
        {
            var detail = string.IsNullOrWhiteSpace(result.CombinedOutput) ? "未返回诊断信息。" : result.CombinedOutput.Trim();
            throw new InvalidOperationException($"{Path.GetFileName(executable)} 执行失败（退出码 {result.ExitCode}）：{detail}");
        }

        return result;
    }

    private static IReadOnlyList<string> WithSerial(string? serial, params string[] arguments)
    {
        var result = new List<string>(arguments.Length + 2);
        if (!string.IsNullOrWhiteSpace(serial))
        {
            result.Add("-s");
            result.Add(serial);
        }

        result.AddRange(arguments);
        return result;
    }

    private static IEnumerable<string> ParseAdbDevices(string output)
    {
        foreach (var line in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            if (line.StartsWith("List of devices", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            var parts = line.Split(['\t', ' '], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
            if (parts.Length >= 2)
            {
                yield return $"{parts[0]}\t{parts[1]}";
            }
        }
    }

    private static IEnumerable<string> ParseFastbootDevices(string output)
    {
        foreach (var line in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            var parts = line.Split(['\t', ' '], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
            if (parts.Length >= 1)
            {
                yield return $"{parts[0]}\tfastboot";
            }
        }
    }

    private static string ExtractGetVarValue(string output, string variable)
    {
        foreach (var originalLine in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            var line = originalLine.StartsWith("(bootloader)", StringComparison.OrdinalIgnoreCase)
                ? originalLine["(bootloader)".Length..].TrimStart()
                : originalLine;
            var prefix = $"{variable}:";
            if (line.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            {
                return line[prefix.Length..].Trim();
            }
        }

        return output.Trim();
    }
}
