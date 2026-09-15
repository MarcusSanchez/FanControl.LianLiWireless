using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

namespace FanControl.LianLiWireless;

/// <summary>
/// The entry points of lianli_wireless.dll. The library sits beside this
/// assembly rather than beside the host, so it is loaded by its full path
/// once before the first call; later calls then resolve to the loaded module.
/// </summary>
internal static class Native
{
    internal const string LibraryName = "lianli_wireless";
    internal const string LibraryFile = "lianli_wireless.dll";

    internal const int Ok = 0;
    internal const int ErrorArgument = -1;
    internal const int ErrorDongle = -2;
    internal const int ErrorLConnect = -3;
    internal const int ErrorPanic = -4;
    internal const int ErrorSize = -5;
    internal const int ErrorWindows = -6;

    private static readonly object LoadLock = new object();
    private static IntPtr _module;

    /// <summary>Loads the library from this assembly's folder. Safe to call repeatedly.</summary>
    internal static void EnsureLoaded()
    {
        lock (LoadLock)
        {
            if (_module != IntPtr.Zero)
            {
                return;
            }

            string path = Path.Combine(AssemblyFolder(), LibraryFile);
            IntPtr module = LoadLibraryW(path);
            if (module == IntPtr.Zero)
            {
                int code = Marshal.GetLastWin32Error();
                throw new DllNotFoundException(
                    "could not load " + path + ": Windows error " + code.ToString(System.Globalization.CultureInfo.InvariantCulture));
            }

            _module = module;
        }
    }

    private static string AssemblyFolder()
    {
        string location = typeof(Native).Assembly.Location;
        string? folder = string.IsNullOrEmpty(location) ? null : Path.GetDirectoryName(location);
        return folder ?? AppContext.BaseDirectory;
    }

    /// <summary>The description of the last failure on this thread.</summary>
    internal static string LastError()
    {
        var buffer = new byte[1024];
        int length = lianli_last_error(buffer, (UIntPtr)buffer.Length);
        return length > 0 ? Encoding.UTF8.GetString(buffer, 0, length) : string.Empty;
    }

    [DllImport("kernel32", CharSet = CharSet.Unicode, SetLastError = true, ExactSpelling = true)]
    private static extern IntPtr LoadLibraryW(string fileName);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint lianli_version();

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int lianli_open(out IntPtr handle);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int lianli_close(IntPtr handle);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int lianli_set_percent(IntPtr handle, byte[] mac, byte percent);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int lianli_clear(IntPtr handle, byte[] mac);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int lianli_read_state(IntPtr handle, byte[] state);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int lianli_take_log(IntPtr handle, byte[] buffer, UIntPtr length);

    [DllImport(LibraryName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int lianli_last_error(byte[] buffer, UIntPtr length);
}
