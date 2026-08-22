using CommunityToolkit.Mvvm.ComponentModel;
using System.Windows.Media;
using VivoKsu.App.Models;

namespace VivoKsu.App.ViewModels;

public partial class DeviceSessionViewModel : ObservableObject
{
    [ObservableProperty]
    private DeviceDetailsSnapshot details = DeviceDetailsSnapshot.Empty;

    [ObservableProperty]
    private DeviceConnectionState connectionState = DeviceConnectionState.Disconnected;

    [ObservableProperty]
    private string deviceName = "未检测到设备";

    [ObservableProperty]
    private string serial = "--";

    [ObservableProperty]
    private string connectionLabel = "等待连接";

    [ObservableProperty]
    private string androidVersion = "--";

    [ObservableProperty]
    private string batteryLevel = "--";

    [ObservableProperty]
    private OperationKind operationKind = OperationKind.Idle;

    [ObservableProperty]
    private string statusText = "未检测到设备";

    [ObservableProperty]
    private bool isBusy;

    public bool IsAdbConnected => ConnectionState == DeviceConnectionState.AdbConnected;

    public Brush ConnectionAccentBrush => ConnectionState switch
    {
        DeviceConnectionState.AdbConnected or DeviceConnectionState.FastbootConnected => new SolidColorBrush(Color.FromRgb(0x0A, 0x8C, 0x86)),
        DeviceConnectionState.Unauthorized or DeviceConnectionState.MultipleDevices or DeviceConnectionState.Error => new SolidColorBrush(Color.FromRgb(0xDA, 0x67, 0x48)),
        _ => new SolidColorBrush(Color.FromRgb(0x7C, 0x8C, 0x92))
    };

    partial void OnConnectionStateChanged(DeviceConnectionState value)
    {
        OnPropertyChanged(nameof(IsAdbConnected));
        OnPropertyChanged(nameof(ConnectionAccentBrush));
    }

    public void BeginOperation(OperationKind kind, string text)
    {
        OperationKind = kind;
        StatusText = text;
        IsBusy = true;
    }

    public void CompleteOperation(string text = "操作完成")
    {
        OperationKind = OperationKind.Completed;
        StatusText = text;
        IsBusy = false;
    }

    public void CancelOperation(string text = "操作已取消")
    {
        OperationKind = OperationKind.Canceled;
        StatusText = text;
        IsBusy = false;
    }

    public void FailOperation(string text)
    {
        OperationKind = OperationKind.Failed;
        StatusText = text;
        IsBusy = false;
    }

    public void ApplyDevice(DeviceSnapshot device)
    {
        ConnectionState = device.ConnectionState;
        DeviceName = device.Model;
        Serial = device.Serial;
        ConnectionLabel = device.ConnectionLabel;
        AndroidVersion = device.AndroidVersion;
        BatteryLevel = device.BatteryLevel;
    }

    public void ApplyDetails(DeviceDetailsSnapshot value)
    {
        Details = value;
        DeviceName = value.Model;
        AndroidVersion = value.AndroidVersion;
    }
}
