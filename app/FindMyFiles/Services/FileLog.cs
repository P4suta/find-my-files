using Serilog;

namespace FindMyFiles.Services;

/// <summary>
/// Static logging facade for the app process. Routes through the Serilog logger
/// installed by <see cref="LogSetup"/>, which writes logfmt lines to
/// <c>…\logs\app.log</c> (resolved by <see cref="AppPaths"/>) — the same dir the
/// scope engine logs into (ADR-0037). The facade keeps a tiny scalar-only
/// surface so call sites can never accidentally destructure (and leak) an
/// object. Best-effort: logging must never become a crash source itself.
/// </summary>
public static class FileLog
{
    /// <summary>Absolute path to the active log file (<c>…\logs\app.log</c>) —
    /// surfaced for the diagnostics "open log folder" affordance and
    /// <see cref="Tail"/>.</summary>
    public static string LogPath => Path.Combine(AppPaths.LogDir, "app.log");

    /// <summary>Absolute path to the crash marker dropped on a fatal exit and
    /// read back on the next launch (see <see cref="WriteCrashMarker"/> /
    /// <see cref="TakeCrashMarker"/>).</summary>
    public static string CrashMarkerPath => Path.Combine(AppPaths.LogDir, "crash.marker");

    /// <summary>Log an informational line under <paramref name="area"/>.</summary>
    /// <param name="area">Subsystem tag, e.g. "notify" or "settings".</param>
    /// <param name="message">The message text.</param>
    public static void Info(string area, string message) =>
        ForArea(area).Information("{Msg}", message);

    /// <summary>Log a debug line under <paramref name="area"/> (suppressed unless
    /// the level is lowered via <c>FMF_LOG</c> / <see cref="LogSetup.LevelSwitch"/>).</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    public static void Debug(string area, string message) =>
        ForArea(area).Debug("{Msg}", message);

    /// <summary>Log a warning under <paramref name="area"/>, optionally appending
    /// the full <paramref name="ex"/> as the <c>err=</c> field.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    /// <param name="ex">Optional exception to record.</param>
    public static void Warn(string area, string message, Exception? ex = null) =>
        ForArea(area).Warning(ex, "{Msg}", message);

    /// <summary>Log an error under <paramref name="area"/>, optionally appending
    /// the full <paramref name="ex"/> as the <c>err=</c> field.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    /// <param name="ex">Optional exception to record.</param>
    public static void Error(string area, string message, Exception? ex = null) =>
        ForArea(area).Error(ex, "{Msg}", message);

    /// <summary>Log one structured informational line carrying explicit logfmt
    /// <paramref name="fields"/> (e.g. the per-query <c>rid</c>/<c>hits</c>
    /// correlation line). Values are logged as scalars only — never
    /// destructured — so no object graph can leak.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text (the trailing <c>msg=</c>).</param>
    /// <param name="fields">Ordered key/value pairs emitted as inline fields.</param>
    public static void Event(string area, string message, params (string Key, object Value)[] fields)
    {
        ArgumentNullException.ThrowIfNull(fields);
        var log = ForArea(area);
        foreach (var (key, value) in fields)
        {
            log = log.ForContext(key, value);
        }

        log.Information("{Msg}", message);
    }

    private static ILogger ForArea(string area) => Log.ForContext("area", area);

    /// <summary>Last <paramref name="lines"/> of the active log — for the
    /// diagnostics clipboard dump.</summary>
    /// <param name="lines">How many trailing lines to return.</param>
    /// <returns>The joined tail, or a placeholder if missing/unreadable.</returns>
    public static string Tail(int lines) => TailFrom(LogPath, lines);

    /// <summary>Last <paramref name="lines"/> lines of <paramref name="logPath"/>,
    /// or a placeholder if missing/unreadable. Path-parameterised for tests. The
    /// Serilog File sink keeps the file open shared-read, so this can read it
    /// while logging continues.</summary>
    /// <param name="logPath">The log file to tail.</param>
    /// <param name="lines">How many trailing lines to return.</param>
    /// <returns>The joined tail (newline-separated), or a placeholder string.</returns>
    internal static string TailFrom(string logPath, int lines)
    {
        try
        {
            if (!File.Exists(logPath))
            {
                return "(no app.log)";
            }

            using var stream = new FileStream(
                logPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
            using var reader = new StreamReader(stream);
            var all = new List<string>();
            while (reader.ReadLine() is { } line)
            {
                all.Add(line);
            }

            return string.Join('\n', all.TakeLast(lines));
        }
        catch (Exception ex)
        {
            return $"(app.log unreadable: {ex.Message})";
        }
    }

    /// <summary>Drop a crash marker (timestamp + <paramref name="detail"/>) so
    /// the next launch can detect an abnormal exit and offer to report it.
    /// Best-effort; failures are swallowed. Kept as a direct synchronous write
    /// (not a log line) so it survives a hard crash that never flushes the
    /// logger.</summary>
    /// <param name="detail">Crash context to record alongside the timestamp.</param>
    public static void WriteCrashMarker(string detail)
    {
        try
        {
            Directory.CreateDirectory(AppPaths.LogDir);
            File.WriteAllText(CrashMarkerPath, $"{DateTimeOffset.Now:O}\n{detail}");
        }
        catch
        {
            // Best-effort: nowhere left to report to.
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
