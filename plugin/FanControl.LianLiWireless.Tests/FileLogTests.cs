using System;
using System.IO;
using FanControl.LianLiWireless;
using Xunit;

namespace FanControl.LianLiWireless.Tests;

public class FileLogTests
{
    [Fact]
    public void StampsLinesAndMovesTheFileAsideWhenFull()
    {
        string folder = Path.Combine(Path.GetTempPath(), "lianli-wireless-test-" + Guid.NewGuid().ToString("N"));
        string path = Path.Combine(folder, "log.txt");
        try
        {
            // Each line is a 19-character stamp, " | ", the text and a line end.
            var log = new FileLog(path, 100);
            log.Write("first");
            log.Write("second");
            string[] lines = File.ReadAllLines(path);
            Assert.Equal(2, lines.Length);
            Assert.EndsWith(" | first", lines[0], StringComparison.Ordinal);
            Assert.Equal(19, lines[0].IndexOf(" | ", StringComparison.Ordinal));
            Assert.False(File.Exists(log.PreviousLocation));

            log.Write("third");
            log.Write("fourth");
            Assert.False(File.Exists(log.PreviousLocation));
            Assert.Equal(4, File.ReadAllLines(path).Length);

            log.Write("fifth");
            Assert.True(File.Exists(log.PreviousLocation));
            string[] previous = File.ReadAllLines(log.PreviousLocation);
            string[] current = File.ReadAllLines(path);
            Assert.Equal(4, previous.Length);
            Assert.EndsWith(" | fourth", previous[3], StringComparison.Ordinal);
            Assert.Single(current);
            Assert.EndsWith(" | fifth", current[0], StringComparison.Ordinal);

            for (int i = 0; i < 4; i++)
            {
                log.Write("more");
            }

            Assert.EndsWith(" | fifth", File.ReadAllLines(log.PreviousLocation)[0], StringComparison.Ordinal);
            Assert.Single(File.ReadAllLines(path));
        }
        finally
        {
            if (Directory.Exists(folder))
            {
                Directory.Delete(folder, true);
            }
        }
    }

    [Fact]
    public void AFailureToWriteIsDropped()
    {
        string folder = Path.Combine(Path.GetTempPath(), "lianli-wireless-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(folder);
        try
        {
            var log = new FileLog(folder);
            log.Write("into a directory");
            Assert.True(Directory.Exists(folder));
        }
        finally
        {
            Directory.Delete(folder, true);
        }
    }
}
