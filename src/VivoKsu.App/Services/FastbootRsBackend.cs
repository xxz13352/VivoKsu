using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class FastbootRsBackend
{
    private readonly IFastbootRsNativeApi nativeApi;

    public FastbootRsBackend(IFastbootRsNativeApi nativeApi)
    {
        this.nativeApi = nativeApi;
    }

    public Task<DeviceSnapshot> DiscoverAsync(CancellationToken cancellationToken)
    {
        return Task.Run(() =>
        {
            cancellationToken.ThrowIfCancellationRequested();
            return FastbootRsDeviceParser.Parse(nativeApi.ListDevices());
        }, cancellationToken);
    }

    public Task<string> ShellAsync(string? serial, string command, CancellationToken cancellationToken, int timeoutMilliseconds = 15000)
    {
        return Task.Run(() => nativeApi.Shell(serial, command, timeoutMilliseconds), cancellationToken);
    }

    public Task<string> GetVarAsync(string? serial, string variable, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.GetVar(serial, variable), cancellationToken);
    }

    public Task RebootAsync(string? serial, string target, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.Reboot(serial, target), cancellationToken);
    }

    public Task FastbootRebootAsync(string? serial, string? target, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.FastbootReboot(serial, target), cancellationToken);
    }

    public Task SetActiveAsync(string? serial, string slot, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.SetActive(serial, slot), cancellationToken);
    }

    public Task PushAsync(string? serial, string localPath, string remotePath, CancellationToken cancellationToken, int timeoutMilliseconds = 15000)
    {
        return Task.Run(() => nativeApi.Push(serial, localPath, remotePath, timeoutMilliseconds), cancellationToken);
    }

    public Task<long> PullAsync(string? serial, string remotePath, string localPath, CancellationToken cancellationToken, int timeoutMilliseconds = 15000)
    {
        return Task.Run(() => nativeApi.Pull(serial, remotePath, localPath, timeoutMilliseconds), cancellationToken);
    }

    public Task<string> InstallAsync(string? serial, string apkPath, bool replace, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.Install(serial, apkPath, replace), cancellationToken);
    }

    public Task FlashAsync(string? serial, string partition, string imagePath, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.Flash(serial, partition, imagePath), cancellationToken);
    }

    public Task EraseAsync(string? serial, string partition, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.Erase(serial, partition), cancellationToken);
    }

    public Task<long> FetchAsync(string? serial, string partition, string outputPath, CancellationToken cancellationToken)
    {
        return Task.Run(() => nativeApi.Fetch(serial, partition, outputPath), cancellationToken);
    }
}
