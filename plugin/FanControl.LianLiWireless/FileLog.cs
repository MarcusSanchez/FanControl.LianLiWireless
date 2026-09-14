using System;
using System.Globalization;
using System.IO;

namespace FanControl.LianLiWireless;

/// <summary>
/// Appends stamped lines to a file under the machine's ProgramData, where
/// the plugin can write whichever account the host runs it as. A failure to
/// write is dropped: the log must never take the host down.
/// </summary>
internal sealed class FileLog
{
    private readonly object _sync = new object();
    private readonly string _path;

    public FileLog(string path)
    {
        _path = path ?? throw new ArgumentNullException(nameof(path));
    }

    /// <summary>The usual place for the log.</summary>
    public static string DefaultPath =>
        Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
            "FanControl",
            "lianli-wireless.log");

    /// <summary>Where this log writes.</summary>
    public string Path_ => _path;

    /// <summary>Appends one line with a local timestamp.</summary>
    public void Write(string line)
    {
        string stamped = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture) + " | " + line;
        lock (_sync)
        {
            try
            {
                string? folder = Path.GetDirectoryName(_path);
                if (!string.IsNullOrEmpty(folder))
                {
                    Directory.CreateDirectory(folder);
                }

                File.AppendAllText(_path, stamped + Environment.NewLine);
            }
#pragma warning disable CA1031 // the log must never throw into the host
            catch (Exception)
            {
            }
#pragma warning restore CA1031
        }
    }
}
