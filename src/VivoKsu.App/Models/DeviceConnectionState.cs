namespace VivoKsu.App.Models;

public enum DeviceConnectionState
{
    Disconnected,
    Unauthorized,
    MultipleDevices,
    AdbConnected,
    FastbootConnected,
    Error
}
