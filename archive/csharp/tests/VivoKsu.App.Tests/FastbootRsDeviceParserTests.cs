using VivoKsu.App.Models;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class FastbootRsDeviceParserTests
{
    [Fact]
    public void Parse_detects_one_adb_device_from_native_output()
    {
        var device = FastbootRsDeviceParser.Parse("1A2B3C4D\tdevice\n");

        Assert.Equal(DeviceConnectionState.AdbConnected, device.ConnectionState);
        Assert.Equal("1A2B3C4D", device.Serial);
        Assert.Equal("ADB 已连接", device.ConnectionLabel);
    }

    [Fact]
    public void Parse_preserves_the_fastbootd_mode_reported_by_the_native_backend()
    {
        var device = FastbootRsDeviceParser.Parse("FAST123\tfastboot (fastbootd)\n");

        Assert.Equal(DeviceConnectionState.FastbootConnected, device.ConnectionState);
        Assert.Equal("FAST123", device.Serial);
        Assert.Equal("Fastbootd 已连接", device.ConnectionLabel);
    }

    [Fact]
    public void Parse_reports_an_offline_device_as_disconnected_instead_of_an_adb_connection()
    {
        var device = FastbootRsDeviceParser.Parse("1A2B3C4D\toffline\n");

        Assert.Equal(DeviceConnectionState.Disconnected, device.ConnectionState);
        Assert.Equal("1A2B3C4D", device.Serial);
    }

    [Fact]
    public void Parse_reports_a_no_permissions_device_as_unauthorized()
    {
        var device = FastbootRsDeviceParser.Parse("1A2B3C4D\tno permissions (user in plugdev group)\n");

        Assert.Equal(DeviceConnectionState.Unauthorized, device.ConnectionState);
        Assert.Equal("1A2B3C4D", device.Serial);
    }

    [Fact]
    public void Parse_does_not_treat_an_unknown_mode_as_a_healthy_adb_connection()
    {
        var device = FastbootRsDeviceParser.Parse("1A2B3C4D\tsome-unknown-state\n");

        Assert.Equal(DeviceConnectionState.Error, device.ConnectionState);
        Assert.Equal("1A2B3C4D", device.Serial);
    }
}
