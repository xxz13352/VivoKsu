using System.Diagnostics;
using System.IO;

namespace VivoKsu.App.Services;

public sealed class SystemProcessRunner : IProcessRunner
{
    public IRunningProcess Start(string executable, IReadOnlyList<string> arguments)
    {
        var process = new Process
        {
            StartInfo = CreateStartInfo(executable, arguments),
            EnableRaisingEvents = true
        };

        if (!process.Start())
        {
            throw new InvalidOperationException("无法启动 scrcpy 进程。");
        }

        return new RunningProcess(process);
    }

    private static ProcessStartInfo CreateStartInfo(string executable, IReadOnlyList<string> arguments)
    {
        var fullPath = Path.GetFullPath(executable);
        var startInfo = new ProcessStartInfo
        {
            FileName = fullPath,
            WorkingDirectory = Path.GetDirectoryName(fullPath) ?? Environment.CurrentDirectory,
            UseShellExecute = false,
            CreateNoWindow = true
        };

        foreach (var argument in arguments)
        {
            startInfo.ArgumentList.Add(argument);
        }

        return startInfo;
    }

    private sealed class RunningProcess : IRunningProcess
    {
        private readonly Process process;

        public RunningProcess(Process process)
        {
            this.process = process;
            process.Exited += (_, _) => Exited?.Invoke(this, EventArgs.Empty);
            // A process can exit between Start and this subscription; synthesize the
            // exit notification so the caller does not leak a stale handle.
            if (process.HasExited)
            {
                Exited?.Invoke(this, EventArgs.Empty);
            }
        }

        public bool HasExited => process.HasExited;
        public event EventHandler? Exited;

        public void Stop()
        {
            if (!process.HasExited)
            {
                process.Kill(true);
            }
        }

        public void Dispose() => process.Dispose();
    }
}
