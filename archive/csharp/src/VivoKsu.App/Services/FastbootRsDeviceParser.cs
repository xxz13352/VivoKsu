using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public static class FastbootRsDeviceParser
{
    public static DeviceSnapshot Parse(string output)
    {
        var devices = output
            .Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Select(line => line.Split('\t', 2, StringSplitOptions.TrimEntries))
            .Where(parts => parts.Length == 2)
            .ToArray();

        if (devices.Length == 0)
        {
            return new DeviceSnapshot(DeviceConnectionState.Disconnected, "--", "等待连接");
        }

        if (devices.Length > 1)
        {
            return new DeviceSnapshot(DeviceConnectionState.MultipleDevices, "--", "检测到多台设备");
        }

        var serial = devices[0][0];
        var mode = devices[0][1].ToLowerInvariant();

        if (mode.Contains("unauthorized", StringComparison.Ordinal))
        {
            return new DeviceSnapshot(DeviceConnectionState.Unauthorized, serial, "等待 USB 调试授权");
        }

        if (mode.Contains("fastbootd", StringComparison.Ordinal))
        {
            return new DeviceSnapshot(DeviceConnectionState.FastbootConnected, serial, "Fastbootd 已连接");
        }

        if (mode.Contains("fastboot", StringComparison.Ordinal))
        {
            return new DeviceSnapshot(DeviceConnectionState.FastbootConnected, serial, "Fastboot 已连接");
        }

        if (mode.Contains("offline", StringComparison.Ordinal))
        {
            return new DeviceSnapshot(DeviceConnectionState.Disconnected, serial, "设备离线");
        }

        if (mode.Contains("no permissions", StringComparison.Ordinal))
        {
            return new DeviceSnapshot(DeviceConnectionState.Unauthorized, serial, "无 USB 调试权限");
        }

        // Only an authorized adb state may map to a healthy ADB connection; any
        // unrecognized mode must not masquerade as a connected device.
        if (mode.StartsWith("device", StringComparison.Ordinal))
        {
            return new DeviceSnapshot(DeviceConnectionState.AdbConnected, serial, "ADB 已连接");
        }

        return new DeviceSnapshot(DeviceConnectionState.Error, serial, "未知设备状态");
    }
}
