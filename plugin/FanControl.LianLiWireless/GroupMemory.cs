using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Text;

namespace FanControl.LianLiWireless;

/// <summary>
/// The fan groups this plugin has ever seen bound to the dongle, kept in a
/// small text file beside the log: one line per group, the address and the
/// fan count. FanControl's configuration names a control and a speed
/// sensor per fan of every group it has been given; a group that is
/// missing when the plugin loads would make that configuration invalid,
/// so the plugin registers every remembered group and leaves the values of
/// an absent one blank until it is heard again. The list is read once and
/// held; the file is written only when a group is added or its fan count
/// changes. Deleting the file forgets them all. A file that cannot be read
/// or written is treated as empty.
/// </summary>
internal sealed class GroupMemory
{
    /// <summary>Name of the file, next to the log.</summary>
    public const string FileName = "lianli-wireless-groups.txt";

    private readonly object _sync = new object();
    private readonly string _path;
    private List<GroupState>? _known;

    public GroupMemory(string path)
    {
        _path = path ?? throw new ArgumentNullException(nameof(path));
    }

    /// <summary>The usual place for the file.</summary>
    public static string DefaultPath =>
        Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
            "FanControl",
            FileName);

    /// <summary>Where this memory is kept.</summary>
    public string Location => _path;

    /// <summary>The groups recorded so far, as offline placeholders, in file order.</summary>
    public List<GroupState> Known
    {
        get
        {
            lock (_sync)
            {
                return new List<GroupState>(Loaded());
            }
        }
    }

    /// <summary>
    /// Adds any group not yet recorded, updates the fan count of one that
    /// changed, and writes the file when either happened. Returns the
    /// addresses added.
    /// </summary>
    public List<string> Remember(IEnumerable<GroupState> groups)
    {
        if (groups is null)
        {
            throw new ArgumentNullException(nameof(groups));
        }

        var added = new List<string>();
        lock (_sync)
        {
            List<GroupState> known = Loaded();
            bool changed = false;
            foreach (GroupState group in groups)
            {
                if (group.FanCount < 1)
                {
                    continue;
                }

                GroupState? recorded = Find(known, group.Address);
                if (recorded is null)
                {
                    known.Add(Placeholder(group.Mac, group.FanCount));
                    added.Add(group.Address);
                }
                else if (recorded.FanCount != group.FanCount)
                {
                    recorded.FanCount = group.FanCount;
                    changed = true;
                }
            }

            if (added.Count > 0 || changed)
            {
                Save(known);
            }
        }

        return added;
    }

    private List<GroupState> Loaded()
    {
        if (_known is null)
        {
            _known = Read();
        }

        return _known;
    }

    private List<GroupState> Read()
    {
        var groups = new List<GroupState>();
        string[] lines;
        try
        {
            if (!File.Exists(_path))
            {
                return groups;
            }

            lines = File.ReadAllLines(_path);
        }
#pragma warning disable CA1031 // an unreadable file is an empty memory, never a crash in the host
        catch (Exception)
        {
            return groups;
        }
#pragma warning restore CA1031

        foreach (string line in lines)
        {
            GroupState? group = Parse(line);
            if (group != null && Find(groups, group.Address) is null)
            {
                groups.Add(group);
            }
        }

        return groups;
    }

    private void Save(List<GroupState> groups)
    {
        var text = new StringBuilder();
        foreach (GroupState group in groups)
        {
            text.Append(group.Address)
                .Append(' ')
                .Append(group.FanCount.ToString(CultureInfo.InvariantCulture))
                .Append('\n');
        }

        try
        {
            string? folder = Path.GetDirectoryName(_path);
            if (!string.IsNullOrEmpty(folder))
            {
                Directory.CreateDirectory(folder);
            }

            File.WriteAllText(_path, text.ToString());
        }
#pragma warning disable CA1031 // a file that cannot be written costs only the memory, never the host
        catch (Exception)
        {
        }
#pragma warning restore CA1031
    }

    private static GroupState? Find(List<GroupState> groups, string address)
    {
        foreach (GroupState group in groups)
        {
            if (group.Address == address)
            {
                return group;
            }
        }

        return null;
    }

    /// <summary>Reads one line, "address fans"; anything else is skipped.</summary>
    internal static GroupState? Parse(string line)
    {
        if (line is null)
        {
            return null;
        }

        string[] parts = line.Trim().Split(new[] { ' ', '\t' }, StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length != 2)
        {
            return null;
        }

        byte[]? mac = ParseMac(parts[0]);
        if (mac is null || !int.TryParse(parts[1], NumberStyles.None, CultureInfo.InvariantCulture, out int fans) || fans < 1 || fans > 4)
        {
            return null;
        }

        return Placeholder(mac, fans);
    }

    private static byte[]? ParseMac(string text)
    {
        string[] parts = text.Split(':');
        if (parts.Length != 6)
        {
            return null;
        }

        var mac = new byte[6];
        for (int i = 0; i < 6; i++)
        {
            if (parts[i].Length != 2 || !byte.TryParse(parts[i], NumberStyles.HexNumber, CultureInfo.InvariantCulture, out mac[i]))
            {
                return null;
            }
        }

        return mac;
    }

    private static GroupState Placeholder(byte[] mac, int fans) =>
        new GroupState
        {
            Mac = (byte[])mac.Clone(),
            FanCount = fans,
            Online = false,
        };
}
