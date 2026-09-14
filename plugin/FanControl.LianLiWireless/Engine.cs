using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace FanControl.LianLiWireless;

/// <summary>
/// A running native engine. Opening finds the dongle and starts the loop;
/// disposing stops it, which sends full speed to every reachable group.
/// </summary>
internal sealed class Engine : IDisposable
{
    private readonly object _sync = new object();
    private readonly byte[] _state = new byte[EngineState.Size];
    private readonly byte[] _line = new byte[1024];
    private IntPtr _handle;

    private Engine(IntPtr handle)
    {
        _handle = handle;
    }

    /// <summary>Whether the L-Connect service holds the dongle, from the last failed open.</summary>
    public static bool LastOpenWasLConnect { get; private set; }

    /// <summary>Finds the dongle and starts the engine.</summary>
    /// <exception cref="InvalidOperationException">The dongle could not be opened.</exception>
    public static Engine Open()
    {
        Native.EnsureLoaded();
        int code = Native.lianli_open(out IntPtr handle);
        LastOpenWasLConnect = code == Native.ErrorLConnect;
        if (code != Native.Ok)
        {
            throw new InvalidOperationException(Describe("open", code));
        }

        return new Engine(handle);
    }

    /// <summary>Asks for a percentage on a group, applied on the engine's next tick.</summary>
    public void SetPercent(byte[] mac, int percent)
    {
        if (mac is null || mac.Length != 6)
        {
            throw new ArgumentException("a group address is six bytes", nameof(mac));
        }

        byte clamped = (byte)Math.Max(0, Math.Min(100, percent));
        lock (_sync)
        {
            if (_handle == IntPtr.Zero)
            {
                return;
            }

            int code = Native.lianli_set_percent(_handle, mac, clamped);
            if (code != Native.Ok)
            {
                throw new InvalidOperationException(Describe("set_percent", code));
            }
        }
    }

    /// <summary>What the engine knew at its last tick.</summary>
    public EngineState ReadState()
    {
        lock (_sync)
        {
            if (_handle == IntPtr.Zero)
            {
                return new EngineState();
            }

            Array.Clear(_state, 0, _state.Length);
            byte[] size = BitConverter.GetBytes((uint)EngineState.Size);
            Array.Copy(size, 0, _state, 0, 4);
            int code = Native.lianli_read_state(_handle, _state);
            if (code != Native.Ok)
            {
                throw new InvalidOperationException(Describe("read_state", code));
            }

            return EngineState.Parse(_state);
        }
    }

    /// <summary>The log lines the engine has produced since the last call.</summary>
    public List<string> TakeLog()
    {
        var lines = new List<string>();
        lock (_sync)
        {
            if (_handle == IntPtr.Zero)
            {
                return lines;
            }

            while (true)
            {
                int length = Native.lianli_take_log(_handle, _line, (UIntPtr)_line.Length);
                if (length <= 0)
                {
                    break;
                }

                lines.Add(Encoding.UTF8.GetString(_line, 0, length));
            }
        }

        return lines;
    }

    /// <summary>Stops the engine after full speed to every reachable group. Safe to call twice.</summary>
    public void Dispose()
    {
        lock (_sync)
        {
            if (_handle == IntPtr.Zero)
            {
                return;
            }

            IntPtr handle = _handle;
            _handle = IntPtr.Zero;
            Native.lianli_close(handle);
        }
    }

    private static string Describe(string call, int code)
    {
        string detail = Native.LastError();
        return call + " failed with code " + code.ToString(CultureInfo.InvariantCulture)
            + (detail.Length > 0 ? ": " + detail : string.Empty);
    }
}
