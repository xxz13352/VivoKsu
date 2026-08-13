using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;

namespace VivoKsu.App.Services;

public sealed class DeviceSessionService : IDeviceRefreshService
{
    private readonly FastbootRsBackend backend;
    private readonly DeviceInfoService deviceInfo;
    private readonly OperationLogService logs;
    private int consecutiveAutomaticDisconnects;

    public DeviceSessionService(
        FastbootRsBackend backend,
        DeviceInfoService deviceInfo,
        OperationLogService logs)
    {
        this.backend = backend;
        this.deviceInfo = deviceInfo;
        this.logs = logs;
    }

    public async Task RefreshAsync(
        DeviceSessionViewModel session,
        CancellationToken cancellationToken,
        bool logActivity = true,
        DeviceRefreshMode refreshMode = DeviceRefreshMode.Manual)
    {
        var isAutomatic = refreshMode == DeviceRefreshMode.Automatic;
        if (isAutomatic && session.IsBusy)
        {
            return;
        }

        if (!isAutomatic)
        {
            session.BeginOperation(OperationKind.Discovering, "正在检测设备");
            if (logActivity)
            {
                logs.Write(OperationLogLevel.Info, "正在检测 ADB / Fastboot 设备。");
            }
        }

        try
        {
            var snapshot = await backend.DiscoverAsync(cancellationToken);
            if (isAutomatic && session.IsBusy)
            {
                return;
            }

            if (ShouldDeferAutomaticDisconnect(session, snapshot, isAutomatic))
            {
                return;
            }

            var knownDetails = session.Details;
            DeviceDetailsSnapshot details;
            var batteryLevel = "--";

            if (snapshot.ConnectionState == DeviceConnectionState.AdbConnected)
            {
                details = await deviceInfo.ReadAdbAsync(snapshot.Serial, cancellationToken);
                batteryLevel = await ReadBatteryLevelAsync(snapshot.Serial, cancellationToken);
            }
            else if (snapshot.ConnectionState == DeviceConnectionState.FastbootConnected)
            {
                details = IsSameDevice(knownDetails, snapshot.Serial)
                    ? knownDetails with { Serial = snapshot.Serial }
                    : DeviceDetailsSnapshot.Empty with
                    {
                    Model = snapshot.Model,
                        Serial = snapshot.Serial
                    };

                details = await deviceInfo.ReadFastbootAsync(details, cancellationToken);
            }
            else
            {
                details = DeviceDetailsSnapshot.Empty;
            }

            if (isAutomatic && session.IsBusy)
            {
                return;
            }

            session.ApplyDevice(snapshot);
            session.ApplyDetails(details);
            session.BatteryLevel = batteryLevel;

            if (!isAutomatic)
            {
                session.CompleteOperation(snapshot.ConnectionLabel);
                if (logActivity)
                {
                    logs.Write(OperationLogLevel.Success, "设备信息已更新。");
                }
            }
        }
        catch (Exception exception)
        {
            if (!isAutomatic)
            {
                session.FailOperation("设备检测失败");
                if (logActivity)
                {
                    logs.Write(OperationLogLevel.Error, exception.Message);
                }
            }
        }
    }

    private bool ShouldDeferAutomaticDisconnect(
        DeviceSessionViewModel session,
        DeviceSnapshot snapshot,
        bool isAutomatic)
    {
        if (!isAutomatic)
        {
            consecutiveAutomaticDisconnects = 0;
            return false;
        }

        if (snapshot.ConnectionState != DeviceConnectionState.Disconnected)
        {
            consecutiveAutomaticDisconnects = 0;
            return false;
        }

        if (session.ConnectionState == DeviceConnectionState.Disconnected)
        {
            return false;
        }

        consecutiveAutomaticDisconnects++;
        return consecutiveAutomaticDisconnects < 2;
    }

    private async Task<string> ReadBatteryLevelAsync(string serial, CancellationToken cancellationToken)
    {
        var output = await backend.ShellAsync(serial, "dumpsys battery", cancellationToken);
        foreach (var line in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            if (!line.StartsWith("level:", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            var value = line["level:".Length..].Trim();
            return int.TryParse(value, out var percentage) && percentage is >= 0 and <= 100
                ? $"{percentage}%"
                : "--";
        }

        return "--";
    }

    private static bool IsSameDevice(DeviceDetailsSnapshot details, string serial) =>
        !string.IsNullOrWhiteSpace(serial) &&
        serial != "--" &&
        string.Equals(details.Serial, serial, StringComparison.Ordinal);
}
