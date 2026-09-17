namespace VivoKsu.App.Services;

public interface IDeviceMonitor
{
    Task StartAsync(CancellationToken cancellationToken = default);

    Task StopAsync();

    Task RefreshManualAsync(bool logActivity, CancellationToken cancellationToken = default);
}
