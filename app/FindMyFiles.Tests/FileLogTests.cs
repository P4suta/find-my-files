using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for the <see cref="FileLog"/> tail reader (the
/// F12 "copy diagnostics" dump depends on it). Formatting and rotation moved to
/// Serilog + <see cref="LogfmtFormatter"/> (ADR-0037) and are covered by
/// <see cref="LogfmtFormatterTests"/>; only the tail stays hand-rolled.</summary>
public sealed class FileLogTests
{
    private static string TempDir() => Directory.CreateTempSubdirectory("fmf-log-").FullName;

    [Fact]
    public void TailFrom_returns_the_last_n_lines()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "app.log");
            File.WriteAllLines(path, ["a", "b", "c", "d", "e"]);

            Assert.Equal("d\ne", FileLog.TailFrom(path, 2));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void TailFrom_missing_file_is_a_placeholder_not_a_throw()
    {
        var missing = Path.Combine(TempDir(), "nope.log");

        Assert.Equal("(no app.log)", FileLog.TailFrom(missing, 10));
    }
}
