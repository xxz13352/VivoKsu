using VivoKsu.App.Models;
using VivoKsu.App.Services;
using VivoKsu.App.ViewModels;
using System.Reflection;

namespace VivoKsu.App.Tests;

public class DeviceSessionServiceTests
{
    [Fact]
    public async Task RefreshAsync_applies_discovered_adb_device_and_its_details()
    {
        var native = new SessionNativeApi();
        var backend = new FastbootRsBackend(native);
        var logs = new OperationLogService();
        var session = new DeviceSessionViewModel();
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend, new FakeFastbootCliRunner()), logs);

        await service.RefreshAsync(session, CancellationToken.None);

        Assert.Equal(DeviceConnectionState.AdbConnected, session.ConnectionState);
        Assert.Equal("RF8", session.Serial);
        Assert.Equal("V2318A", session.Details.Model);
        Assert.Equal("78%", session.BatteryLevel);
        Assert.Contains(logs.Entries, entry => entry.Level == OperationLogLevel.Success);
    }

    [Fact]
    public async Task RefreshAsync_clears_stale_details_and_keeps_the_disconnected_status()
    {
        var backend = new FastbootRsBackend(new EmptySessionNativeApi());
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "OLD", "ADB 已连接"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Model = "旧设备", Serial = "OLD" });
        session.BatteryLevel = "88%";
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend, new FakeFastbootCliRunner()), new OperationLogService());

        await service.RefreshAsync(session, CancellationToken.None, logActivity: false);

        Assert.Equal(DeviceConnectionState.Disconnected, session.ConnectionState);
        Assert.Equal("未检测到设备", session.Details.Model);
        Assert.Equal("等待连接", session.StatusText);
        Assert.Equal("--", session.BatteryLevel);
    }

    [Fact]
    public async Task Automatic_refresh_keeps_the_known_adb_device_after_one_empty_discovery()
    {
        var backend = new FastbootRsBackend(new EmptySessionNativeApi());
        var session = CreateConnectedSession();
        session.CompleteOperation("ADB 已连接");
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend, new FakeFastbootCliRunner()), new OperationLogService());

        await InvokeAutomaticRefreshAsync(service, session);

        Assert.Equal(DeviceConnectionState.AdbConnected, session.ConnectionState);
        Assert.Equal("OLD", session.Serial);
        Assert.Equal("旧设备", session.Details.Model);
        Assert.Equal("ADB 已连接", session.StatusText);
    }

    [Fact]
    public async Task Automatic_refresh_marks_the_device_disconnected_after_two_consecutive_empty_discoveries()
    {
        var backend = new FastbootRsBackend(new EmptySessionNativeApi());
        var session = CreateConnectedSession();
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend, new FakeFastbootCliRunner()), new OperationLogService());

        await InvokeAutomaticRefreshAsync(service, session);
        await InvokeAutomaticRefreshAsync(service, session);

        Assert.Equal(DeviceConnectionState.Disconnected, session.ConnectionState);
        Assert.Equal("未检测到设备", session.Details.Model);
        Assert.Equal("--", session.BatteryLevel);
    }

    [Fact]
    public async Task Automatic_refresh_does_not_query_devices_while_a_foreground_operation_is_active()
    {
        var native = new CountingEmptySessionNativeApi();
        var backend = new FastbootRsBackend(native);
        var session = CreateConnectedSession();
        session.BeginOperation(OperationKind.Flashing, "正在刷写 boot");
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend, new FakeFastbootCliRunner()), new OperationLogService());

        await InvokeAutomaticRefreshAsync(service, session);

        Assert.Equal(0, native.ListDevicesCallCount);
        Assert.Equal(DeviceConnectionState.AdbConnected, session.ConnectionState);
        Assert.True(session.IsBusy);
    }

    [Fact]
    public async Task RefreshAsync_keeps_known_adb_details_when_the_same_device_enters_fastboot()
    {
        var backend = new FastbootRsBackend(new FastbootSessionNativeApi());
        var session = new DeviceSessionViewModel();
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with
        {
            Serial = "FAST123",
            Model = "V2318A",
            AndroidVersion = "15",
            FirmwareVersion = "OriginOS 5",
            KernelVersion = "5.15.94"
        });
        var fake = new FakeFastbootCliRunner
        {
            GetVarHandler = variable => variable switch
            {
                "current-slot" => "b",
                "unlocked" => "yes",
                "product" => "V2318A",
                _ => string.Empty
            }
        };
        var service = new DeviceSessionService(backend, new DeviceInfoService(backend, fake), new OperationLogService());

        await service.RefreshAsync(session, CancellationToken.None, logActivity: false);

        Assert.Equal(DeviceConnectionState.FastbootConnected, session.ConnectionState);
        Assert.Equal("V2318A", session.Details.Model);
        Assert.Equal("15", session.Details.AndroidVersion);
        Assert.Equal("OriginOS 5", session.Details.FirmwareVersion);
        Assert.Equal("b", session.Details.ActiveSlot);
        Assert.Equal("unlocked", session.Details.BootloaderState);
    }

    private sealed class SessionNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => "RF8\tdevice\n";
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => command switch
        {
            "getprop" => "[ro.product.brand]: [vivo]\n[ro.product.model]: [V2318A]\n[ro.product.device]: [PD2307]\n[ro.build.version.release]: [15]\n[ro.build.display.id]: [OriginOS 5]\n",
            "dumpsys battery" => "AC powered: false\nlevel: 78\nstatus: 3\n",
            _ => string.Empty
        };
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class EmptySessionNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => string.Empty;
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private sealed class FastbootSessionNativeApi : IFastbootRsNativeApi
    {
        public string ListDevices() => "FAST123\tfastboot\n";
        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;
        public string GetVar(string? serial, string variable) => variable switch
        {
            "current-slot" => "b",
            "unlocked" => "yes",
            "product" => "V2318A",
            _ => string.Empty
        };
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }

    private static DeviceSessionViewModel CreateConnectedSession()
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "OLD", "ADB 已连接", "旧设备", "15", "88%"));
        session.ApplyDetails(DeviceDetailsSnapshot.Empty with { Model = "旧设备", Serial = "OLD", AndroidVersion = "15" });
        return session;
    }

    private static Task InvokeAutomaticRefreshAsync(DeviceSessionService service, DeviceSessionViewModel session)
    {
        var method = typeof(DeviceSessionService).GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .SingleOrDefault(candidate =>
                candidate.Name == nameof(DeviceSessionService.RefreshAsync) &&
                candidate.GetParameters() is { Length: 4 } parameters &&
                parameters[3].ParameterType.Name == "DeviceRefreshMode");

        Assert.NotNull(method);
        var refreshMode = Enum.Parse(method!.GetParameters()[3].ParameterType, "Automatic");
        return Assert.IsAssignableFrom<Task>(method.Invoke(service, [session, CancellationToken.None, false, refreshMode]));
    }

    private sealed class CountingEmptySessionNativeApi : IFastbootRsNativeApi
    {
        public int ListDevicesCallCount { get; private set; }

        public string ListDevices()
        {
            ListDevicesCallCount++;
            return string.Empty;
        }

        public string Shell(string? serial, string command, int timeoutMilliseconds = 15000) => string.Empty;
        public string GetVar(string? serial, string variable) => string.Empty;
        public void Reboot(string? serial, string target) { }
        public void Push(string? serial, string localPath, string remotePath) { }
        public long Pull(string? serial, string remotePath, string localPath) => 0;
        public string Install(string? serial, string apkPath, bool replace) => string.Empty;
        public void Flash(string? serial, string partition, string imagePath) { }
    }
}
