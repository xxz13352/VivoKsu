namespace VivoKsu.App.Services;

public interface IProcessRunner
{
    IRunningProcess Start(
        string executable,
        IReadOnlyList<string> arguments,
        IReadOnlyDictionary<string, string>? environment = null);
}

public interface IRunningProcess : IDisposable
{
    bool HasExited { get; }
    event EventHandler? Exited;
    void Stop();
}
