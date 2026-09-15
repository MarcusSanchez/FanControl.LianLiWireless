using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace FanControl.LianLiWireless;

/// <summary>A native call failed; carries the library's code.</summary>
internal sealed class EngineException : Exception
{
    public EngineException(string call, int code, string detail)
        : base(call + " failed with code " + code.ToString(CultureInfo.InvariantCulture)
            + (detail.Length > 0 ? ": " + detail : string.Empty))
    {
        Code = code;
    }

    /// <summary>The library's error code.</summary>
    public int Code { get; }

    /// <summary>Whether the failure was the L-Connect service holding the dongle.</summary>
    public bool IsLConnect => Code == Native.ErrorLConnect;
}

/// <summary>
/// A running native engine. Opening finds the dongle and starts the loop;
/// disposing stops it and leaves the groups at their last duty.
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

    ~Engine()
    {
        Dispose();
    }

    /// <summary>Finds the dongle and starts the engine.</summary>
    /// <exception cref="EngineException">The dongle could not be opened.</exception>
    public static Engine Open()
    {
        Native.EnsureLoaded();
        uint version = Native.lianli_version();
        if (version != EngineState.Version)
        {
            throw new EngineException("version", Native.ErrorSize, "library speaks interface " + version.ToString(CultureInfo.InvariantCulture) + ", this plugin expects " + EngineState.Version.ToString(CultureInfo.InvariantCulture));
        }

        int code = Native.lianli_open(out IntPtr handle);
        if (code != Native.Ok)
        {
            throw new EngineException("open", code, Native.LastError());
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
                throw new EngineException("set_percent", code, Native.LastError());
            }
        }
    }

    /// <summary>Stops driving a group on the engine's next tick. Its fans keep their duty.</summary>
    public void Clear(byte[] mac)
    {
        if (mac is null || mac.Length != 6)
        {
            throw new ArgumentException("a group address is six bytes", nameof(mac));
        }

        lock (_sync)
        {
            if (_handle == IntPtr.Zero)
            {
                return;
            }

            int code = Native.lianli_clear(_handle, mac);
            if (code != Native.Ok)
            {
                throw new EngineException("clear", code, Native.LastError());
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
                throw new EngineException("read_state", code, Native.LastError());
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

    /// <summary>Stops the engine. Safe to call twice.</summary>
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

        GC.SuppressFinalize(this);
    }
}
