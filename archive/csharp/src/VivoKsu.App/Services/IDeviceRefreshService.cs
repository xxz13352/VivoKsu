using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public interface IDeviceRefreshService
{
    Task RefreshAsync(
        DeviceSessionViewModel session,
        CancellationToken cancellationToken,
        bool logActivity = true,
        DeviceRefreshMode refreshMode = DeviceRefreshMode.Manual);
}
