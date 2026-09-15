using System;
using System.Collections.Generic;
using System.Globalization;

namespace FanControl.LianLiWireless;

/// <summary>One fan group as the engine last reported it.</summary>
internal sealed class GroupState
{
    /// <summary>Bytes of the group's address.</summary>
    public byte[] Mac { get; set; } = new byte[6];

    /// <summary>Fans attached to the group.</summary>
    public int FanCount { get; set; }

    /// <summary>Whether the group has been heard recently.</summary>
    public bool Online { get; set; }

    /// <summary>Whether the receiver reports the target applied.</summary>
    public bool Acknowledged { get; set; }

    /// <summary>Speed of each fan, four slots.</summary>
    public int[] Rpm { get; set; } = new int[4];

    /// <summary>Duty the receiver reports for each fan, four slots.</summary>
    public int[] Duty { get; set; } = new int[4];

    /// <summary>The address as colon-separated hex.</summary>
    public string Address => EngineState.FormatMac(Mac);
}

/// <summary>What the engine knew at its last tick, read from the native state layout.</summary>
internal sealed class EngineState
{
    /// <summary>Bytes the native state structure occupies.</summary>
    public const int Size = 560;

    /// <summary>The interface version this layout belongs to.</summary>
    public const uint Version = 1;

    private const int GroupSize = 32;
    private const int MaxGroups = 16;
    private const int GroupsOffset = 44;

    /// <summary>Ticks the engine has run.</summary>
    public ulong Ticks { get; set; }

    /// <summary>Polls that succeeded.</summary>
    public ulong Polls { get; set; }

    /// <summary>Polls that failed.</summary>
    public ulong PollFailures { get; set; }

    /// <summary>Whether the failsafe is in force.</summary>
    public bool Alarm { get; set; }

    /// <summary>Fan groups in slot order.</summary>
    public IReadOnlyList<GroupState> Groups { get; set; } = Array.Empty<GroupState>();

    /// <summary>Reads a state from the bytes the library filled.</summary>
    public static EngineState Parse(byte[] buffer)
    {
        if (buffer is null)
        {
            throw new ArgumentNullException(nameof(buffer));
        }

        if (buffer.Length < Size)
        {
            throw new ArgumentException("state buffer is " + buffer.Length.ToString(CultureInfo.InvariantCulture) + " bytes, need " + Size.ToString(CultureInfo.InvariantCulture), nameof(buffer));
        }

        uint version = BitConverter.ToUInt32(buffer, 4);
        if (version != Version)
        {
            throw new ArgumentException("state is interface version " + version.ToString(CultureInfo.InvariantCulture) + ", this plugin reads " + Version.ToString(CultureInfo.InvariantCulture), nameof(buffer));
        }

        int count = (int)Math.Min(BitConverter.ToUInt32(buffer, 40), MaxGroups);
        var groups = new List<GroupState>(count);
        for (int i = 0; i < count; i++)
        {
            int at = GroupsOffset + i * GroupSize;
            var group = new GroupState
            {
                FanCount = buffer[at + 7],
                Online = buffer[at + 9] != 0,
                Acknowledged = buffer[at + 10] != 0,
            };
            Array.Copy(buffer, at, group.Mac, 0, 6);
            for (int slot = 0; slot < 4; slot++)
            {
                group.Rpm[slot] = BitConverter.ToUInt16(buffer, at + 16 + slot * 2);
                group.Duty[slot] = buffer[at + 24 + slot];
            }

            groups.Add(group);
        }

        return new EngineState
        {
            Ticks = BitConverter.ToUInt64(buffer, 16),
            Polls = BitConverter.ToUInt64(buffer, 24),
            PollFailures = BitConverter.ToUInt64(buffer, 32),
            Alarm = buffer[15] != 0,
            Groups = groups,
        };
    }

    /// <summary>An address as colon-separated lowercase hex.</summary>
    public static string FormatMac(byte[] mac)
    {
        if (mac is null)
        {
            throw new ArgumentNullException(nameof(mac));
        }

        var parts = new string[mac.Length];
        for (int i = 0; i < mac.Length; i++)
        {
            parts[i] = mac[i].ToString("x2", CultureInfo.InvariantCulture);
        }

        return string.Join(":", parts);
    }
}
