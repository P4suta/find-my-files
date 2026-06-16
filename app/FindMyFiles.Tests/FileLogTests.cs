using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural tests for <see cref="FileLog"/> — formatting, rotation
/// and tail, exercised against a temp directory instead of %APPDATA% via the
/// path-parameterised internal cores. Previously the logger had no tests at all
/// (it is the "黙らない" backstop, so it must actually work).</summary>
public sealed class FileLogTests
{
    private static string TempDir() => Directory.CreateTempSubdirectory("fmf-log-").FullName;

    [Fact]
    public void FormatLine_carries_level_area_and_message_on_one_line()
    {
        var line = FileLog.FormatLine(
            "WARN",
            "shell",
            "boom",
            null,
            new DateTimeOffset(2026, 6, 15, 1, 2, 3, TimeSpan.Zero));

        Assert.Contains("[WARN] [shell] boom", line, StringComparison.Ordinal);
        Assert.Contains("2026-06-15T01:02:03", line, StringComparison.Ordinal);
        Assert.DoesNotContain('\n', line);
    }

    [Fact]
    public void FormatLine_appends_the_exception_after_a_separator()
    {
        var line = FileLog.FormatLine(
            "ERROR",
            "x",
            "msg",
            new InvalidOperationException("kaboom"),
            DateTimeOffset.UnixEpoch);

        Assert.Contains(" ── ", line, StringComparison.Ordinal);
        Assert.Contains("kaboom", line, StringComparison.Ordinal);
    }

    [Fact]
    public void AppendLine_writes_into_the_dir_app_log_with_a_trailing_newline()
    {
        var dir = TempDir();
        try
        {
            FileLog.AppendLine(dir, "hello world");

            var text = File.ReadAllText(Path.Combine(dir, "app.log"));
            Assert.Equal("hello world" + Environment.NewLine, text);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void RotateIfNeeded_moves_an_oversized_log_to_old()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "app.log");
            File.WriteAllText(path, new string('x', 200));

            FileLog.RotateIfNeeded(path, rotateBytes: 100);

            Assert.False(File.Exists(path));
            Assert.True(File.Exists(path + ".old"));
            Assert.Equal(200, new FileInfo(path + ".old").Length);
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

    [Fact]
    public void RotateIfNeeded_leaves_a_small_log_in_place()
    {
        var dir = TempDir();
        try
        {
            var path = Path.Combine(dir, "app.log");
            File.WriteAllText(path, "small");

            FileLog.RotateIfNeeded(path, rotateBytes: 1000);

            Assert.True(File.Exists(path));
            Assert.False(File.Exists(path + ".old"));
        }
        finally
        {
            Directory.Delete(dir, recursive: true);
        }
    }

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
