namespace VivoKsu.App.Models;

public sealed record DeviceSnapshot(
    DeviceConnectionState ConnectionState,
    string Serial,
    string ConnectionLabel,
    string Model = "未检测到设备",
    string AndroidVersion = "--",
    string BatteryLevel = "--");
