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

    public static string LogPath => Path.Combine(LogDir, "app.log");
    public static string CrashMarkerPath => Path.Combine(LogDir, "crash.marker");

    private const long RotateBytes = 2 * 1024 * 1024;

    public static void Info(string area, string message) => Write("INFO", area, message, null);

    public static void Warn(string area, string message, Exception? ex = null) =>
        Write("WARN", area, message, ex);

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
                sb.Append('[').Append(DateTimeOffset.Now.ToString("yyyy-MM-ddTHH:mm:ss.fffzzz"))
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
