using VivoKsu.App.Models;
using VivoKsu.App.ViewModels;
using System.Windows.Media;

namespace VivoKsu.App.Tests;

public class DeviceSessionViewModelTests
{
    [Fact]
    public void IsAdbConnected_tracks_the_current_connection_state()
    {
        var session = new DeviceSessionViewModel();

        Assert.False(session.IsAdbConnected);

        session.ApplyDevice(new DeviceSnapshot(DeviceConnectionState.AdbConnected, "ADB001", "设备已连接"));

        Assert.True(session.IsAdbConnected);
    }

    [Theory]
    [InlineData(DeviceConnectionState.Disconnected, "#7C8C92")]
    [InlineData(DeviceConnectionState.AdbConnected, "#0A8C86")]
    [InlineData(DeviceConnectionState.FastbootConnected, "#0A8C86")]
    [InlineData(DeviceConnectionState.Unauthorized, "#DA6748")]
    public void ConnectionAccentBrush_matches_the_connection_semantics(DeviceConnectionState state, string expected)
    {
        var session = new DeviceSessionViewModel();
        session.ApplyDevice(new DeviceSnapshot(state, "STATE", "状态"));

        var actual = Assert.IsType<SolidColorBrush>(session.ConnectionAccentBrush).Color;

        Assert.Equal((Color)ColorConverter.ConvertFromString(expected), actual);
    }
}
