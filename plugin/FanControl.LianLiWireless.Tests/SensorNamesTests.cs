using FanControl.LianLiWireless;
using Xunit;

namespace FanControl.LianLiWireless.Tests;

public class SensorNamesTests
{
    private const string Address = "7c:9c:06:f5:17:e1";

    [Fact]
    public void IdentifiersCarryTheFullAddress()
    {
        Assert.Equal("lianli-wireless/7c:9c:06:f5:17:e1/control", SensorNames.ControlId(Address));
        Assert.Equal("lianli-wireless/7c:9c:06:f5:17:e1/fan1", SensorNames.FanId(Address, 0));
        Assert.Equal("lianli-wireless/7c:9c:06:f5:17:e1/fan3", SensorNames.FanId(Address, 2));
    }

    [Fact]
    public void NamesUseTheShortAddress()
    {
        Assert.Equal("Wireless 7c:9c:06 (3 fans)", SensorNames.ControlName(Address, 3));
        Assert.Equal("Wireless 7c:9c:06 (1 fan)", SensorNames.ControlName(Address, 1));
        Assert.Equal("Wireless 7c:9c:06 fan 2", SensorNames.FanName(Address, 1));
        Assert.Equal("ab", SensorNames.Short("ab"));
    }

    [Fact]
    public void SensorsTakeTheirNamesFromTheGroup()
    {
        var group = new GroupState
        {
            Mac = new byte[] { 0x7c, 0x9c, 0x06, 0xf5, 0x17, 0xe1 },
            FanCount = 3,
        };
        string logPath = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "lianli-wireless-test-" + System.Guid.NewGuid().ToString("N") + ".log");
        using var plugin = new WirelessPlugin(null, new FileLog(logPath));
        try
        {
            RunSensorChecks(plugin, group);
        }
        finally
        {
            System.IO.File.Delete(logPath);
        }
    }

    private static void RunSensorChecks(WirelessPlugin plugin, GroupState group)
    {
        var control = new GroupControl(plugin, group);
        var fan = new FanSensor(plugin, group, 1);
        Assert.Equal("lianli-wireless/7c:9c:06:f5:17:e1/control", control.Id);
        Assert.Equal("Wireless 7c:9c:06 (3 fans)", control.Name);
        Assert.Equal("Wireless 7c:9c:06 fan 2", fan.Name);
        Assert.Null(control.Value);
        control.Set(55.4f);
        control.Update();
        Assert.Equal(55.4f, control.Value);
        Assert.Equal(55, plugin.Asked("7c:9c:06:f5:17:e1"));
        control.Reset();
        control.Update();
        Assert.Null(control.Value);
        Assert.Equal(55, plugin.Asked("7c:9c:06:f5:17:e1"));
        Assert.Null(plugin.Asked("99:db:c8:e5:66:e1"));
        fan.Update();
        Assert.Null(fan.Value);
    }
}
