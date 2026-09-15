using System;
using System.Collections.Generic;
using System.Globalization;
using System.Threading;
using FanControl.Plugins;

namespace FanControl.LianLiWireless;

/// <summary>
/// FanControl entry point for Lian Li wireless fans. The native engine owns
/// the dongle and runs the fan loop on its own thread; this type starts and
/// stops it, hands it the percentages FanControl asks for, and publishes
/// what it reports as sensors.
/// </summary>
public sealed class WirelessPlugin : IPlugin2, IDisposable
{
    private static readonly TimeSpan FirstStateWait = TimeSpan.FromSeconds(5);

    private readonly object _sync = new object();
    private readonly IPluginLogger? _host;
    private readonly FileLog _file;
    private readonly Dictionary<string, int> _asked = new Dictionary<string, int>(StringComparer.Ordinal);
    private Engine? _engine;
    private EngineState _state = new EngineState();
    private bool _alarmShown;

    /// <summary>Host-injected constructor. FanControl supplies the logger.</summary>
    public WirelessPlugin(IPluginLogger logger)
        : this(logger, new FileLog(FileLog.DefaultPath))
    {
    }

    internal WirelessPlugin(IPluginLogger? host, FileLog file)
    {
        _host = host;
        _file = file ?? throw new ArgumentNullException(nameof(file));
    }

    /// <summary>Plugin name shown in FanControl.</summary>
    public string Name => "Lian Li Wireless";

    /// <summary>Opens the dongle and starts the engine. A failure is logged and leaves no sensors.</summary>
    public void Initialize()
    {
        lock (_sync)
        {
            TearDown();
            try
            {
                _engine = Engine.Open();
                Log("engine opened; log at " + _file.Location);
            }
            catch (EngineException ex)
            {
                Log(ex.IsLConnect
                    ? "not started: " + ex.Message + "; stop the L-Connect service and refresh the plugin"
                    : "not started: " + ex.Message);
                _engine = null;
                return;
            }
#pragma warning disable CA1031 // host seam: a library that will not load leaves the plugin empty, never crashes FanControl
            catch (Exception ex)
            {
                Log("not started: " + ex.Message);
                _engine = null;
                return;
            }
#pragma warning restore CA1031

            WaitForFirstState();
            ReapplyAsked();
        }
    }

    /// <summary>
    /// Hands the engine the percentages FanControl last asked for. FanControl
    /// closes and reopens the plugin on every refresh, and does not repeat a
    /// control's value afterwards until it changes, so the fresh engine would
    /// otherwise have no targets.
    /// </summary>
    private void ReapplyAsked()
    {
        if (_engine is null || _asked.Count == 0)
        {
            return;
        }

        foreach (GroupState group in _state.Groups)
        {
            if (_asked.TryGetValue(group.Address, out int percent))
            {
                Ask(group.Mac, percent);
            }
        }
    }

    /// <summary>The percentage FanControl last asked for on a group, if any.</summary>
    internal int? Asked(string address)
    {
        lock (_sync)
        {
            return _asked.TryGetValue(address, out int percent) ? percent : (int?)null;
        }
    }

    private void WaitForFirstState()
    {
        DateTime deadline = DateTime.UtcNow + FirstStateWait;
        while (DateTime.UtcNow < deadline)
        {
            Refresh();
            if (_state.Ticks > 0 && _state.Groups.Count > 0)
            {
                break;
            }

            Thread.Sleep(200);
        }

        Log(string.Format(
            CultureInfo.InvariantCulture,
            "{0} fan group(s) after {1} tick(s)",
            _state.Groups.Count,
            _state.Ticks));
    }

    /// <summary>Registers one control per group and one speed sensor per fan.</summary>
    public void Load(IPluginSensorsContainer container)
    {
        if (container is null)
        {
            throw new ArgumentNullException(nameof(container));
        }

        lock (_sync)
        {
            foreach (GroupState group in _state.Groups)
            {
                container.ControlSensors.Add(new GroupControl(this, group));
                for (int slot = 0; slot < group.FanCount; slot++)
                {
                    container.FanSensors.Add(new FanSensor(this, group, slot));
                }
            }
        }
    }

    /// <summary>Reads the engine's latest state and drains its log.</summary>
    public void Update()
    {
        lock (_sync)
        {
            Refresh();
        }
    }

    /// <summary>Stops the engine. The groups keep their last duty.</summary>
    public void Close()
    {
        lock (_sync)
        {
            TearDown();
        }
    }

    /// <summary>Same as <see cref="Close"/>.</summary>
    public void Dispose()
    {
        Close();
    }

    internal void Ask(byte[] mac, int percent)
    {
        lock (_sync)
        {
            _asked[EngineState.FormatMac(mac)] = percent;
            try
            {
                _engine?.SetPercent(mac, percent);
            }
#pragma warning disable CA1031 // host seam: a refused request is logged, the control stays
            catch (Exception ex)
            {
                Log("set failed: " + ex.Message);
            }
#pragma warning restore CA1031
        }
    }

    internal float? Rpm(string address, int slot)
    {
        lock (_sync)
        {
            foreach (GroupState group in _state.Groups)
            {
                if (group.Address == address)
                {
                    return group.Online && slot < group.Rpm.Length ? group.Rpm[slot] : (float?)null;
                }
            }

            return null;
        }
    }

    private void Refresh()
    {
        if (_engine is null)
        {
            return;
        }

        try
        {
            foreach (string line in _engine.TakeLog())
            {
                _file.Write(line);
            }

            _state = _engine.ReadState();
            if (_state.Alarm != _alarmShown)
            {
                _alarmShown = _state.Alarm;
                Log(_state.Alarm ? "failsafe in force: every reachable group at full speed" : "failsafe cleared");
            }
        }
#pragma warning disable CA1031 // host seam: a failed read keeps the last state, never crashes FanControl
        catch (Exception ex)
        {
            Log("read failed: " + ex.Message);
        }
#pragma warning restore CA1031
    }

    private void TearDown()
    {
        if (_engine is null)
        {
            return;
        }

        Log("closing; the groups keep their last duty");
        try
        {
            foreach (string line in _engine.TakeLog())
            {
                _file.Write(line);
            }
        }
#pragma warning disable CA1031 // closing must finish even if the last log drain fails
        catch (Exception)
        {
        }
#pragma warning restore CA1031

        _engine.Dispose();
        _engine = null;
        _state = new EngineState();
        _alarmShown = false;
    }

    private void Log(string line)
    {
        _file.Write(line);
        try
        {
            _host?.Log(Name + ": " + line);
        }
#pragma warning disable CA1031 // the host's logger must not take the plugin down
        catch (Exception)
        {
        }
#pragma warning restore CA1031
    }
}
