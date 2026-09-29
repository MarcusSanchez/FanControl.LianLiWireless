using System;
using System.Collections.Generic;
using System.IO;
using FanControl.LianLiWireless;
using FanControl.Plugins;
using Xunit;

namespace FanControl.LianLiWireless.Tests;

public class GroupMemoryTests
{
    private static readonly byte[] A = { 0x7c, 0x9c, 0x06, 0xf5, 0x17, 0xe1 };
    private static readonly byte[] B = { 0x99, 0xdb, 0xc8, 0xe5, 0x66, 0xe1 };

    private static string TempPath() =>
        Path.Combine(Path.GetTempPath(), "lianli-wireless-test-" + Guid.NewGuid().ToString("N"), "groups.txt");

    [Fact]
    public void AMissingFileIsAnEmptyMemory()
    {
        var memory = new GroupMemory(TempPath());
        Assert.Empty(memory.Known);
    }

    [Fact]
    public void RemembersNewGroupsAndKeepsOldOnes()
    {
        string path = TempPath();
        try
        {
            var memory = new GroupMemory(path);
            var live = new List<GroupState> { new GroupState { Mac = A, FanCount = 3, Online = true } };
            Assert.Equal(new[] { "7c:9c:06:f5:17:e1" }, memory.Remember(live));
            Assert.Empty(memory.Remember(live));
            Assert.Equal("7c:9c:06:f5:17:e1 3\n", File.ReadAllText(path));

            var later = new List<GroupState> { new GroupState { Mac = B, FanCount = 2, Online = true } };
            Assert.Equal(new[] { "99:db:c8:e5:66:e1" }, memory.Remember(later));

            List<GroupState> known = new GroupMemory(path).Known;
            Assert.Equal(2, known.Count);
            Assert.Equal("7c:9c:06:f5:17:e1", known[0].Address);
            Assert.Equal(3, known[0].FanCount);
            Assert.False(known[0].Online);
            Assert.Equal("99:db:c8:e5:66:e1", known[1].Address);
            Assert.Equal(2, known[1].FanCount);
        }
        finally
        {
            Directory.Delete(Path.GetDirectoryName(path)!, true);
        }
    }

    [Fact]
    public void UpdatesAChangedFanCountAndWritesOnlyThen()
    {
        string path = TempPath();
        try
        {
            var memory = new GroupMemory(path);
            memory.Remember(new List<GroupState> { new GroupState { Mac = A, FanCount = 2, Online = true } });
            DateTime written = File.GetLastWriteTimeUtc(path);
            Assert.Empty(memory.Remember(new List<GroupState> { new GroupState { Mac = A, FanCount = 2, Online = true } }));
            Assert.Equal(written, File.GetLastWriteTimeUtc(path));
            Assert.Empty(memory.Remember(new List<GroupState> { new GroupState { Mac = A, FanCount = 3, Online = true } }));
            Assert.Equal("7c:9c:06:f5:17:e1 3\n", File.ReadAllText(path));
            Assert.Equal(3, memory.Known[0].FanCount);
            Assert.Empty(memory.Remember(new List<GroupState> { new GroupState { Mac = A, FanCount = 0, Online = false } }));
            Assert.Equal(3, memory.Known[0].FanCount);
        }
        finally
        {
            Directory.Delete(Path.GetDirectoryName(path)!, true);
        }
    }

    [Fact]
    public void SkipsLinesItCannotRead()
    {
        Assert.Null(GroupMemory.Parse(""));
        Assert.Null(GroupMemory.Parse("7c:9c:06:f5:17:e1"));
        Assert.Null(GroupMemory.Parse("7c:9c:06:f5:17 3"));
        Assert.Null(GroupMemory.Parse("7c:9c:06:f5:17:zz 3"));
        Assert.Null(GroupMemory.Parse("7c:9c:06:f5:17:e1 0"));
        Assert.Null(GroupMemory.Parse("7c:9c:06:f5:17:e1 5"));
        GroupState? group = GroupMemory.Parse("  7c:9c:06:f5:17:e1   2 ");
        Assert.NotNull(group);
        Assert.Equal("7c:9c:06:f5:17:e1", group!.Address);
        Assert.Equal(2, group.FanCount);
    }

    [Fact]
    public void ARememberedGroupGetsItsSensorsWhileAbsent()
    {
        string path = TempPath();
        string logPath = Path.Combine(Path.GetDirectoryName(path)!, "log.txt");
        try
        {
            var memory = new GroupMemory(path);
            memory.Remember(new List<GroupState> { new GroupState { Mac = A, FanCount = 3, Online = true } });
            using var plugin = new WirelessPlugin(null, new FileLog(logPath), memory);
            var container = new Container();
            plugin.Load(container);

            Assert.Single(container.ControlSensors);
            Assert.Equal("lianli-wireless/7c:9c:06:f5:17:e1/control", container.ControlSensors[0].Id);
            Assert.Equal("Wireless 7c:9c:06 (3 fans)", container.ControlSensors[0].Name);
            Assert.Equal(3, container.FanSensors.Count);
            Assert.Equal("lianli-wireless/7c:9c:06:f5:17:e1/fan3", container.FanSensors[2].Id);
            container.ControlSensors[0].Update();
            container.FanSensors[0].Update();
            Assert.Null(container.ControlSensors[0].Value);
            Assert.Null(container.FanSensors[0].Value);
            container.ControlSensors[0].Set(40f);
            Assert.Equal(40, plugin.Asked("7c:9c:06:f5:17:e1"));
        }
        finally
        {
            Directory.Delete(Path.GetDirectoryName(path)!, true);
        }
    }

    private sealed class Container : IPluginSensorsContainer
    {
        public List<IPluginControlSensor> ControlSensors { get; } = new List<IPluginControlSensor>();

        public List<IPluginSensor> FanSensors { get; } = new List<IPluginSensor>();

        public List<IPluginSensor> TempSensors { get; } = new List<IPluginSensor>();
    }
}
