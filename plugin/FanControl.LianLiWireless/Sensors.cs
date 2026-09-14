using System;
using System.Globalization;
using FanControl.Plugins;

namespace FanControl.LianLiWireless;

/// <summary>Names and identifiers for a group's sensors, stable across runs.</summary>
internal static class SensorNames
{
    private const string Prefix = "lianli-wireless";

    /// <summary>Identifier of a group's control.</summary>
    public static string ControlId(string address) => Prefix + "/" + address + "/control";

    /// <summary>Display name of a group's control.</summary>
    public static string ControlName(string address, int fanCount) =>
        "Wireless " + Short(address) + " (" + fanCount.ToString(CultureInfo.InvariantCulture) + (fanCount == 1 ? " fan)" : " fans)");

    /// <summary>Identifier of one fan's speed sensor.</summary>
    public static string FanId(string address, int slot) =>
        Prefix + "/" + address + "/fan" + (slot + 1).ToString(CultureInfo.InvariantCulture);

    /// <summary>Display name of one fan's speed sensor.</summary>
    public static string FanName(string address, int slot) =>
        "Wireless " + Short(address) + " fan " + (slot + 1).ToString(CultureInfo.InvariantCulture);

    /// <summary>The first three bytes of an address, enough to tell groups apart.</summary>
    public static string Short(string address)
    {
        if (address is null)
        {
            throw new ArgumentNullException(nameof(address));
        }

        return address.Length >= 8 ? address.Substring(0, 8) : address;
    }
}

/// <summary>
/// The control for one group. Setting hands the percentage to the engine;
/// the value shown is what was last asked for. Resetting leaves the fans at
/// their last duty, since nothing else is there to take them.
/// </summary>
internal sealed class GroupControl : IPluginControlSensor
{
    private readonly WirelessPlugin _plugin;
    private readonly byte[] _mac;
    private float? _asked;

    public GroupControl(WirelessPlugin plugin, GroupState group)
    {
        _plugin = plugin ?? throw new ArgumentNullException(nameof(plugin));
        if (group is null)
        {
            throw new ArgumentNullException(nameof(group));
        }

        _mac = (byte[])group.Mac.Clone();
        Address = group.Address;
        Id = SensorNames.ControlId(Address);
        Name = SensorNames.ControlName(Address, group.FanCount);
    }

    /// <summary>The group's address.</summary>
    public string Address { get; }

    public string Id { get; }

    public string Name { get; }

    public float? Value { get; private set; }

    public void Update() => Value = _asked;

    public void Set(float val)
    {
        _asked = val;
        _plugin.Ask(_mac, (int)Math.Round(val));
    }

    public void Reset()
    {
        _asked = null;
    }
}

/// <summary>The speed of one fan, taken from the engine's last state.</summary>
internal sealed class FanSensor : IPluginSensor
{
    private readonly WirelessPlugin _plugin;
    private readonly string _address;
    private readonly int _slot;

    public FanSensor(WirelessPlugin plugin, GroupState group, int slot)
    {
        _plugin = plugin ?? throw new ArgumentNullException(nameof(plugin));
        if (group is null)
        {
            throw new ArgumentNullException(nameof(group));
        }

        _address = group.Address;
        _slot = slot;
        Id = SensorNames.FanId(_address, slot);
        Name = SensorNames.FanName(_address, slot);
    }

    public string Id { get; }

    public string Name { get; }

    public float? Value { get; private set; }

    public void Update() => Value = _plugin.Rpm(_address, _slot);
}
