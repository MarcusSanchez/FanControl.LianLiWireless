using System;
using System.Globalization;
using System.IO;

namespace FanControl.LianLiWireless;

/// <summary>
/// Appends stamped lines to a file under the machine's ProgramData, where
/// the plugin can write whichever account the host runs it as. The file is
/// opened per line, so it can be read, moved or deleted at any time. When
/// it passes <see cref="MaxBytes"/> it is moved aside once, replacing the
/// previous copy, and a fresh file is started. A failure to write is
/// dropped: the log must never take the host down.
/// </summary>
internal sealed class FileLog
{
    /// <summary>Size at which the file is moved aside.</summary>
    public const long MaxBytes = 1024 * 1024;

    private readonly object _sync = new object();
    private readonly string _path;
    private readonly long _maxBytes;

    public FileLog(string path)
        : this(path, MaxBytes)
    {
    }

    internal FileLog(string path, long maxBytes)
    {
        _path = path ?? throw new ArgumentNullException(nameof(path));
        _maxBytes = maxBytes;
    }

    /// <summary>The usual place for the log.</summary>
    public static string DefaultPath =>
        Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
            "FanControl",
            "lianli-wireless.log");

    /// <summary>Where this log writes.</summary>
    public string Location => _path;

    /// <summary>Where the previous log goes when the file is moved aside.</summary>
    public string PreviousLocation => _path + ".1";

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

                MoveAsideWhenFull();
                File.AppendAllText(_path, stamped + Environment.NewLine);
            }
#pragma warning disable CA1031 // the log must never throw into the host
            catch (Exception)
            {
            }
#pragma warning restore CA1031
        }
    }

    /// <summary>
    /// Moves a full file to <see cref="PreviousLocation"/>. A move that
    /// fails, say because something holds the previous file open, is
    /// tried again on the next line; the current file keeps growing until
    /// then rather than losing lines.
    /// </summary>
    private void MoveAsideWhenFull()
    {
        try
        {
            var info = new FileInfo(_path);
            if (!info.Exists || info.Length < _maxBytes)
            {
                return;
            }

            if (File.Exists(PreviousLocation))
            {
                File.Delete(PreviousLocation);
            }

            File.Move(_path, PreviousLocation);
        }
#pragma warning disable CA1031 // a failed move must not cost the line being written
        catch (Exception)
        {
        }
#pragma warning restore CA1031
    }
}
