using System.Globalization;
using System.Text;

namespace FindMyFiles.Services;

/// <summary>
/// Zero-dependency file logger for the app process. Covers everything on the
/// C# side; the log directory is resolved by <see cref="AppPaths"/> (portable
/// <c>&lt;exe&gt;\data\logs</c> by default, else <c>%APPDATA%\find-my-files\logs</c>) —
/// the same dir the scope engine logs into. Thread-safe, single rotation
/// generation.
/// </summary>
public static class FileLog
{
    private const long RotateBytes = 2 * 1024 * 1024;

    private static readonly System.Threading.Lock Gate = new();
    private static readonly string LogDir = AppPaths.LogDir;

    /// <summary>Absolute path to the active log file (<c>…\logs\app.log</c> under
    /// the resolved data root) — surfaced for the diagnostics "open log folder"
    /// affordance.</summary>
    public static string LogPath => Path.Combine(LogDir, "app.log");

    /// <summary>Absolute path to the crash marker dropped on a fatal exit and
    /// read back on the next launch (see <see cref="WriteCrashMarker"/> /
    /// <see cref="TakeCrashMarker"/>).</summary>
    public static string CrashMarkerPath => Path.Combine(LogDir, "crash.marker");

    /// <summary>Log an informational line under <paramref name="area"/> (the
    /// subsystem tag shown in brackets).</summary>
    /// <param name="area">Subsystem tag, e.g. "notify" or "settings".</param>
    /// <param name="message">The message text.</param>
    public static void Info(string area, string message) => Write("INFO", area, message, null);

    /// <summary>Log a warning under <paramref name="area"/>, optionally
    /// appending the full <paramref name="ex"/> for the stack trace.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    /// <param name="ex">Optional exception to append verbatim.</param>
    public static void Warn(string area, string message, Exception? ex = null) =>
        Write("WARN", area, message, ex);

    /// <summary>Log an error under <paramref name="area"/>, optionally
    /// appending the full <paramref name="ex"/> for the stack trace.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    /// <param name="ex">Optional exception to append verbatim.</param>
    public static void Error(string area, string message, Exception? ex = null) =>
        Write("ERROR", area, message, ex);

    /// <summary>Best-effort: logging must never become a crash source itself.</summary>
    private static void Write(string level, string area, string message, Exception? ex)
    {
        try
        {
            lock (Gate)
            {
                AppendLine(LogDir, FormatLine(level, area, message, ex, DateTimeOffset.Now));
            }
        }
        catch
        {
            // Swallow: nowhere left to report to.
        }
    }

    /// <summary>Format one log line (no trailing newline) — pure, so the exact
    /// shape is unit-testable without touching the filesystem.</summary>
    /// <param name="level">Level tag (INFO/WARN/ERROR).</param>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    /// <param name="ex">Optional exception appended verbatim after " ── ".</param>
    /// <param name="now">Timestamp to stamp (injected for determinism).</param>
    /// <returns>The formatted line.</returns>
    internal static string FormatLine(string level, string area, string message, Exception? ex, DateTimeOffset now)
    {
        var sb = new StringBuilder();
        sb.Append('[').Append(now.ToString("yyyy-MM-ddTHH:mm:ss.fffzzz", CultureInfo.InvariantCulture))
          .Append("] [").Append(level).Append("] [").Append(area).Append("] ")
          .Append(message);
        if (ex is not null)
        {
            sb.Append(" ── ").Append(ex);
        }

        return sb.ToString();
    }

    /// <summary>Append a preformatted line to <c>&lt;dir&gt;\app.log</c>, rotating
    /// first. Takes the directory so tests can target a temp dir instead of
    /// %APPDATA%.</summary>
    /// <param name="dir">Log directory (created if absent).</param>
    /// <param name="line">The preformatted line (the newline is appended here).</param>
    internal static void AppendLine(string dir, string line)
    {
        Directory.CreateDirectory(dir);
        var path = Path.Combine(dir, "app.log");
        RotateIfNeeded(path, RotateBytes);
        File.AppendAllText(path, line + Environment.NewLine);
    }

    /// <summary>Move <paramref name="logPath"/> to its <c>.old</c> sibling once it
    /// exceeds <paramref name="rotateBytes"/> (single rotation generation).</summary>
    /// <param name="logPath">The active log file.</param>
    /// <param name="rotateBytes">Size threshold in bytes.</param>
    internal static void RotateIfNeeded(string logPath, long rotateBytes)
    {
        var info = new FileInfo(logPath);
        if (info.Exists && info.Length > rotateBytes)
        {
            var old = logPath + ".old";
            File.Delete(old);
            File.Move(logPath, old);
        }
    }

    /// <summary>Last `lines` of the log — for the diagnostics clipboard dump.</summary>
    /// <param name="lines">How many trailing lines to return.</param>
    /// <returns>The joined tail, or a placeholder if missing/unreadable.</returns>
    public static string Tail(int lines) => TailFrom(LogPath, lines);

    /// <summary>Last <paramref name="lines"/> lines of <paramref name="logPath"/>,
    /// or a placeholder if missing/unreadable. Path-parameterised for tests.</summary>
    /// <param name="logPath">The log file to tail.</param>
    /// <param name="lines">How many trailing lines to return.</param>
    /// <returns>The joined tail (newline-separated), or a placeholder string.</returns>
    internal static string TailFrom(string logPath, int lines)
    {
        try
        {
            lock (Gate)
            {
                if (!File.Exists(logPath))
                {
                    return "(no app.log)";
                }

                var all = File.ReadAllLines(logPath);
                return string.Join('\n', all.TakeLast(lines));
            }
        }
        catch (Exception ex)
        {
            return $"(app.log unreadable: {ex.Message})";
        }
    }

    /// <summary>Drop a crash marker (timestamp + <paramref name="detail"/>) so
    /// the next launch can detect an abnormal exit and offer to report it.
    /// Best-effort; failures are swallowed.</summary>
    /// <param name="detail">Crash context to record alongside the timestamp.</param>
    public static void WriteCrashMarker(string detail)
    {
        try
        {
            Directory.CreateDirectory(LogDir);
            File.WriteAllText(CrashMarkerPath, $"{DateTimeOffset.Now:O}\n{detail}");
        }
        catch
        {
        }
    }

    /// <summary>Returns and clears the crash marker from the previous run.</summary>
    /// <returns>The marker contents, or <c>null</c> if absent or unreadable.</returns>
    public static string? TakeCrashMarker()
    {
        try
        {
            if (!File.Exists(CrashMarkerPath))
            {
                return null;
            }

            var text = File.ReadAllText(CrashMarkerPath);
            File.Delete(CrashMarkerPath);
            return text;
        }
        catch
        {
            return null;
        }
    }
}
