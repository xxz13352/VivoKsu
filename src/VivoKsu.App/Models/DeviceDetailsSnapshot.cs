namespace VivoKsu.App.Models;

public sealed record DeviceDetailsSnapshot(
    string Brand,
    string Model,
    string Codename,
    string Serial,
    string AndroidVersion,
    string FirmwareVersion,
    string KernelVersion,
    string ActiveSlot,
    string BootloaderState,
    string VerifiedBootState,
    string UsbDebuggingState,
    string BuildTime)
{
    public static DeviceDetailsSnapshot Empty { get; } = new(
        "--", "未检测到设备", "--", "--", "--", "--", "--", "--", "--", "--", "--", "--");
}
