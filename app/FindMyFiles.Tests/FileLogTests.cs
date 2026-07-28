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

    [Fact]
    public void SafeTailFrom_redacts_messages_and_arbitrary_fields()
    {
        var dir = TempDir();
        try
        {
            const string secret = "C:\\Users\\alice\\secret-query.txt";
            var path = Path.Combine(dir, "app.log");
            File.WriteAllText(
                path,
                $"2026-07-25T00:00:00.000+09:00 INFO area=query qlen=7 path={secret} msg=\"{secret}\"");

            var safe = FileLog.SafeTailFrom(path, 10);

            Assert.Contains("area=query", safe, StringComparison.Ordinal);
            Assert.Contains("qlen=7", safe, StringComparison.Ordinal);
            Assert.DoesNotContain(secret, safe, StringComparison.Ordinal);
            Assert.EndsWith("msg=\"[redacted]\"", safe, StringComparison.Ordinal);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void Crash_marker_never_serializes_exception_message_or_stack()
    {
        const string secret = @"C:\Users\alice\secret-query.txt";
        var ex = new InvalidOperationException(secret);

        var marker = FileLog.FormatCrashMarker(
            new DateTimeOffset(2026, 7, 25, 0, 0, 0, TimeSpan.Zero),
            CrashReason.FatalAppDomainException,
            ex);

        Assert.Contains("reason=fatal_appdomain_exception", marker, StringComparison.Ordinal);
        Assert.Contains("error_type=System.InvalidOperationException", marker, StringComparison.Ordinal);
        Assert.Contains("hresult=0x", marker, StringComparison.Ordinal);
        Assert.DoesNotContain(secret, marker, StringComparison.Ordinal);
        Assert.DoesNotContain(" at ", marker, StringComparison.Ordinal);
    }

    [Fact]
    public void Crash_marker_reader_is_bounded_strict_utf8_and_consuming()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "crash.marker");
            File.WriteAllText(path, "reason=fatal_appdomain_exception");

            Assert.Equal(
                "reason=fatal_appdomain_exception",
                FileLog.TakeCrashMarkerFrom(path));
            Assert.False(File.Exists(path));

            File.WriteAllBytes(path, new byte[4097]);
            Assert.Null(FileLog.TakeCrashMarkerFrom(path));
            Assert.False(File.Exists(path));

            File.WriteAllBytes(path, [0xFF]);
            Assert.Null(FileLog.TakeCrashMarkerFrom(path));
            Assert.False(File.Exists(path));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }
}
