using System;
using FanControl.LianLiWireless;
using Xunit;

namespace FanControl.LianLiWireless.Tests;

public class EngineStateTests
{
    private static byte[] Buffer(uint groupCount, params (byte[] mac, byte fans, bool online, bool acknowledged, ushort[] rpm, byte[] duty)[] groups)
    {
        var buffer = new byte[EngineState.Size];
        BitConverter.GetBytes((uint)EngineState.Size).CopyTo(buffer, 0);
        BitConverter.GetBytes(1u).CopyTo(buffer, 4);
        buffer[14] = 8;
        buffer[15] = 0;
        BitConverter.GetBytes(3449UL).CopyTo(buffer, 16);
        BitConverter.GetBytes(3449UL).CopyTo(buffer, 24);
        BitConverter.GetBytes(2UL).CopyTo(buffer, 32);
        BitConverter.GetBytes(groupCount).CopyTo(buffer, 40);
        for (int i = 0; i < groups.Length; i++)
        {
            int at = 44 + i * 32;
            var g = groups[i];
            g.mac.CopyTo(buffer, at);
            buffer[at + 6] = 2;
            buffer[at + 7] = g.fans;
            buffer[at + 8] = 46;
            buffer[at + 9] = (byte)(g.online ? 1 : 0);
            buffer[at + 10] = (byte)(g.acknowledged ? 1 : 0);
            buffer[at + 11] = 1;
            BitConverter.GetBytes(0u).CopyTo(buffer, at + 12);
            for (int slot = 0; slot < 4; slot++)
            {
                BitConverter.GetBytes(g.rpm[slot]).CopyTo(buffer, at + 16 + slot * 2);
                buffer[at + 24 + slot] = g.duty[slot];
            }
        }

        return buffer;
    }

    [Fact]
    public void ParsesCountersAndGroups()
    {
        byte[] a = { 0x7c, 0x9c, 0x06, 0xf5, 0x17, 0xe1 };
        byte[] b = { 0x99, 0xdb, 0xc8, 0xe5, 0x66, 0xe1 };
        byte[] buffer = Buffer(
            2,
            (a, 3, true, true, new ushort[] { 1765, 1767, 1771, 0 }, new byte[] { 206, 206, 206, 0 }),
            (b, 2, false, false, new ushort[] { 0, 0, 0, 0 }, new byte[] { 206, 206, 0, 0 }));

        EngineState state = EngineState.Parse(buffer);

        Assert.Equal(3449UL, state.Ticks);
        Assert.Equal(3449UL, state.Polls);
        Assert.Equal(2UL, state.PollFailures);
        Assert.False(state.Alarm);
        Assert.Equal(2, state.Groups.Count);
        Assert.Equal("7c:9c:06:f5:17:e1", state.Groups[0].Address);
        Assert.Equal(3, state.Groups[0].FanCount);
        Assert.True(state.Groups[0].Online);
        Assert.True(state.Groups[0].Acknowledged);
        Assert.Equal(new[] { 1765, 1767, 1771, 0 }, state.Groups[0].Rpm);
        Assert.Equal(new[] { 206, 206, 206, 0 }, state.Groups[0].Duty);
        Assert.Equal("99:db:c8:e5:66:e1", state.Groups[1].Address);
        Assert.False(state.Groups[1].Online);
    }

    [Fact]
    public void ReadsTheAlarmFlagAndCapsTheGroupCount()
    {
        byte[] buffer = Buffer(200);
        buffer[15] = 1;
        EngineState state = EngineState.Parse(buffer);
        Assert.True(state.Alarm);
        Assert.Equal(16, state.Groups.Count);
    }

    [Fact]
    public void RefusesAShortBuffer()
    {
        Assert.Throws<ArgumentException>(() => EngineState.Parse(new byte[100]));
        Assert.Throws<ArgumentNullException>(() => EngineState.Parse(null!));
    }

    [Fact]
    public void RefusesAnotherInterfaceVersion()
    {
        byte[] buffer = Buffer(0);
        BitConverter.GetBytes(2u).CopyTo(buffer, 4);
        var error = Assert.Throws<ArgumentException>(() => EngineState.Parse(buffer));
        Assert.Contains("version 2", error.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void FormatsAddresses()
    {
        Assert.Equal("bf:3d:ca:e5:66:e4", EngineState.FormatMac(new byte[] { 0xbf, 0x3d, 0xca, 0xe5, 0x66, 0xe4 }));
        Assert.Equal("00:01", EngineState.FormatMac(new byte[] { 0, 1 }));
    }
}
