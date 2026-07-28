using System.ComponentModel;
using System.Globalization;
using Serilog;

namespace FindMyFiles.Services;

/// <summary>
/// Static logging facade for the app process. Routes through the Serilog logger
/// installed by <see cref="LogSetup"/>, which writes logfmt lines to
/// <c>…\logs\app.log</c> (resolved by <see cref="AppPaths"/>) — the same dir the
/// scope engine logs into (ADR-0037). The facade keeps a tiny scalar-only
/// surface so call sites can never accidentally destructure (and leak) an
/// object. Exception text and stack traces are deliberately never persisted:
/// they routinely contain user paths, query text, pipe names, and machine
/// details. Only type/HRESULT/native error code cross this boundary.
/// Best-effort: logging must never become a crash source itself.
/// </summary>
internal static class FileLog
{
    private const int MaxCrashMarkerBytes = 4096;
    private static readonly System.Text.Encoding StrictUtf8 =
        new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true);

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
    /// privacy-safe exception metadata.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    /// <param name="ex">Optional exception to record.</param>
    public static void Warn(string area, string message, Exception? ex = null) =>
        WithFailure(ForArea(area), ex).Warning("{Msg}", message);

    /// <summary>Log an error under <paramref name="area"/>, optionally appending
    /// privacy-safe exception metadata.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text.</param>
    /// <param name="ex">Optional exception to record.</param>
    public static void Error(string area, string message, Exception? ex = null) =>
        WithFailure(ForArea(area), ex).Error("{Msg}", message);

    /// <summary>Log one structured informational line carrying explicit logfmt
    /// <paramref name="fields"/> (e.g. the per-query <c>rid</c>/<c>hits</c>
    /// correlation line). Values are logged as scalars only — never
    /// destructured — so no object graph can leak.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">The message text (the trailing <c>msg=</c>).</param>
    /// <param name="fields">Ordered key/value pairs emitted as inline fields.</param>
    public static void Event(string area, string message, params (string Key, object Value)[] fields)
    {
        WithFields(ForArea(area), fields).Information("{Msg}", message);
    }

    /// <summary>Warning counterpart of <see cref="Event"/>. Exception content
    /// is reduced to non-sensitive scalar metadata.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">Constant, non-sensitive message text.</param>
    /// <param name="ex">Optional exception reduced to scalar metadata.</param>
    /// <param name="fields">Explicit scalar fields.</param>
    internal static void WarnEvent(
        string area,
        string message,
        Exception? ex = null,
        params (string Key, object Value)[] fields) =>
        WithFailure(WithFields(ForArea(area), fields), ex).Warning("{Msg}", message);

    /// <summary>Error counterpart of <see cref="Event"/>. Exception content
    /// is reduced to non-sensitive scalar metadata.</summary>
    /// <param name="area">Subsystem tag.</param>
    /// <param name="message">Constant, non-sensitive message text.</param>
    /// <param name="ex">Optional exception reduced to scalar metadata.</param>
    /// <param name="fields">Explicit scalar fields.</param>
    internal static void ErrorEvent(
        string area,
        string message,
        Exception? ex = null,
        params (string Key, object Value)[] fields) =>
        WithFailure(WithFields(ForArea(area), fields), ex).Error("{Msg}", message);

    private static ILogger ForArea(string area) => Log.ForContext("area", area);

    private static ILogger WithFields(
        ILogger log,
        (string Key, object Value)[] fields)
    {
        ArgumentNullException.ThrowIfNull(fields);
        foreach (var (key, value) in fields)
        {
            log = log.ForContext(key, value);
        }

        return log;
    }

    private static ILogger WithFailure(ILogger log, Exception? ex)
    {
        if (ex is null)
        {
            return log;
        }

        log = log
            .ForContext("error_type", ex.GetType().FullName ?? ex.GetType().Name)
            .ForContext("hresult", $"0x{ex.HResult:X8}");
        return ex is Win32Exception win32
            ? log.ForContext("win32", win32.NativeErrorCode)
            : log;
    }

    /// <summary>Last <paramref name="lines"/> of the active log — for the
    /// diagnostics clipboard dump.</summary>
    /// <param name="lines">How many trailing lines to return.</param>
    /// <returns>The joined tail, or a placeholder if missing/unreadable.</returns>
    public static string Tail(int lines) => TailFrom(LogPath, lines);

    /// <summary>Privacy-safe log tail for copied diagnostics. It keeps only
    /// timestamp, level, area and explicitly whitelisted scalar counters;
    /// messages and arbitrary fields are always redacted.</summary>
    /// <param name="lines">How many trailing records to inspect.</param>
    /// <returns>A redacted, newline-separated log tail.</returns>
    public static string SafeTail(int lines) => SafeTailFrom(LogPath, lines);

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
            _ = ex;
            return "(app.log unreadable)";
        }
    }

    internal static string SafeTailFrom(string logPath, int lines)
    {
        var tail = TailFrom(logPath, lines);
        if (tail.StartsWith('('))
        {
            return tail;
        }

        return string.Join(
            '\n',
            tail.Split('\n').Select(SanitizeLogLine));
    }

    private static string SanitizeLogLine(string line)
    {
        var tokens = line.Split(' ', StringSplitOptions.RemoveEmptyEntries);
        if (tokens.Length < 3)
        {
            return "(redacted log line)";
        }

        var safe = new List<string> { tokens[0], tokens[1] };
        safe.AddRange(tokens.Skip(2).Where(IsSafeDiagnosticToken));
        safe.Add("msg=\"[redacted]\"");
        return string.Join(' ', safe);
    }

    private static bool IsSafeDiagnosticToken(string token)
    {
        var split = token.IndexOf('=', StringComparison.Ordinal);
        if (split <= 0 || split == token.Length - 1)
        {
            return false;
        }

        var key = token[..split];
        var value = token[(split + 1)..];
        if (string.Equals(key, "area", StringComparison.Ordinal))
        {
            return value.All(ch => char.IsAsciiLetterOrDigit(ch) || ch is '-' or '_' or '.');
        }

        if (key is not ("rid" or "qid" or "hits" or "qlen" or "dur_us"
            or "volumes" or "reconnects" or "entries" or "page"))
        {
            return false;
        }

        return ulong.TryParse(value, NumberStyles.None, CultureInfo.InvariantCulture, out _);
    }

    /// <summary>Drop a privacy-safe crash marker so the next launch can detect
    /// an abnormal exit and offer to report it.
    /// Best-effort; failures are swallowed. Kept as a direct synchronous write
    /// (not a log line) so it survives a hard crash that never flushes the
    /// logger. Exception messages and stack traces never cross this boundary.</summary>
    /// <param name="reason">Finite crash category.</param>
    /// <param name="ex">Optional exception reduced to scalar metadata.</param>
    public static void WriteCrashMarker(CrashReason reason, Exception? ex)
    {
        try
        {
            Directory.CreateDirectory(AppPaths.LogDir);
            File.WriteAllText(
                CrashMarkerPath,
                FormatCrashMarker(DateTimeOffset.Now, reason, ex));
        }
        catch
        {
            // Best-effort: nowhere left to report to.
        }
    }

    /// <summary>Pure crash-marker serializer, exposed internally for a
    /// regression test that pins the privacy boundary.</summary>
    /// <param name="timestamp">Crash time.</param>
    /// <param name="reason">Finite crash category.</param>
    /// <param name="ex">Optional exception reduced to scalar metadata.</param>
    /// <returns>The newline-separated marker body.</returns>
    internal static string FormatCrashMarker(
        DateTimeOffset timestamp,
        CrashReason reason,
        Exception? ex)
    {
        var lines = new List<string>
        {
            timestamp.ToString("O", CultureInfo.InvariantCulture),
            $"reason={reason switch
            {
                CrashReason.XamlExceptionStorm => "xaml_exception_storm",
                CrashReason.FatalAppDomainException => "fatal_appdomain_exception",
                _ => "unknown",
            }}",
        };
        if (ex is not null)
        {
            lines.Add($"error_type={ex.GetType().FullName ?? ex.GetType().Name}");
            lines.Add($"hresult=0x{ex.HResult:X8}");
            if (ex is Win32Exception win32)
            {
                lines.Add($"win32={win32.NativeErrorCode}");
            }
        }

        return string.Join('\n', lines);
    }

    /// <summary>Returns and clears the crash marker from the previous run.</summary>
    /// <returns>The marker contents, or <c>null</c> if absent or unreadable.</returns>
    public static string? TakeCrashMarker() => TakeCrashMarkerFrom(CrashMarkerPath);

    internal static string? TakeCrashMarkerFrom(string path)
    {
        try
        {
            if (!File.Exists(path))
            {
                return null;
            }

            string text;
            using (var stream = new FileStream(
                path,
                FileMode.Open,
                FileAccess.Read,
                FileShare.Read,
                bufferSize: 1024,
                FileOptions.SequentialScan))
            {
                if (stream.Length > MaxCrashMarkerBytes)
                {
                    throw new InvalidDataException(
                        $"crash marker exceeds {MaxCrashMarkerBytes} bytes");
                }

                var bytes = new byte[checked((int)stream.Length)];
                stream.ReadExactly(bytes);
                if (stream.ReadByte() != -1)
                {
                    throw new InvalidDataException(
                        $"crash marker exceeds {MaxCrashMarkerBytes} bytes");
                }

                text = StrictUtf8.GetString(bytes);
            }

            File.Delete(path);
            return text;
        }
        catch (Exception ex)
        {
            try
            {
                File.Delete(path);
            }
            catch (Exception cleanupError)
            {
                Warn("crash-marker", "could not discard invalid crash marker", cleanupError);
            }

            Warn("crash-marker", "could not consume previous crash marker", ex);
            return null;
        }
    }
}

internal enum CrashReason
{
    XamlExceptionStorm,
    FatalAppDomainException,
}
