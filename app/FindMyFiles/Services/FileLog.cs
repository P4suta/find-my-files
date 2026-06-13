using System.Globalization;
using System.Text;

namespace FindMyFiles.Services;

/// <summary>
/// Zero-dependency file logger for the app process. Engine-side logs go to
/// %ProgramData%\find-my-files\logs\engine.log; this covers everything that
/// happens on the C# side. Thread-safe, single rotation generation.
/// </summary>
public static class FileLog
{
    private static readonly object Gate = new();
    private static readonly string LogDir = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "find-my-files", "logs");

    /// <summary>Absolute path to the active log file
    /// (<c>%APPDATA%\find-my-files\logs\app.log</c>) — surfaced for the
    /// diagnostics "open log folder" affordance.</summary>
    public static string LogPath => Path.Combine(LogDir, "app.log");

    /// <summary>Absolute path to the crash marker dropped on a fatal exit and
    /// read back on the next launch (see <see cref="WriteCrashMarker"/> /
    /// <see cref="TakeCrashMarker"/>).</summary>
    public static string CrashMarkerPath => Path.Combine(LogDir, "crash.marker");

    private const long RotateBytes = 2 * 1024 * 1024;

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
                Directory.CreateDirectory(LogDir);
                RotateIfNeeded();
                var sb = new StringBuilder();
                sb.Append('[').Append(DateTimeOffset.Now.ToString("yyyy-MM-ddTHH:mm:ss.fffzzz", CultureInfo.InvariantCulture))
                  .Append("] [").Append(level).Append("] [").Append(area).Append("] ")
                  .Append(message);
                if (ex is not null)
                {
                    sb.Append(" ── ").Append(ex);
                }
                sb.AppendLine();
                File.AppendAllText(LogPath, sb.ToString());
            }
        }
        catch
        {
            // Swallow: nowhere left to report to.
        }
    }

    private static void RotateIfNeeded()
    {
        var info = new FileInfo(LogPath);
        if (info.Exists && info.Length > RotateBytes)
        {
            var old = LogPath + ".old";
            File.Delete(old);
            File.Move(LogPath, old);
        }
    }

    /// <summary>Last `lines` of the log — for the diagnostics clipboard dump.</summary>
    public static string Tail(int lines)
    {
        try
        {
            lock (Gate)
            {
                if (!File.Exists(LogPath))
                {
                    return "(no app.log)";
                }
                var all = File.ReadAllLines(LogPath);
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
