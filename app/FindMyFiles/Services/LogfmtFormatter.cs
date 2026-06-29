using System.Globalization;
using System.Text;
using Serilog.Events;
using Serilog.Formatting;

namespace FindMyFiles.Services;

/// <summary>
/// Serilog text formatter that renders each event as one logfmt line —
/// <c>ts level area key=value … msg="…"</c> — matching the Rust engine's
/// <c>engine.log</c> so the two logs interleave and parse identically
/// (ADR-0037). The quoting rules double as the log-injection defence: any
/// value carrying CR/LF or control characters is escaped, so it can never
/// forge a second log line.
/// </summary>
public sealed class LogfmtFormatter : ITextFormatter
{
    /// <summary>Cap on one field value's length, mirroring the engine's 1 KiB
    /// cap so a pathological filename/query cannot balloon a line.</summary>
    private const int ValueCap = 1024;

    /// <summary>Property holding the subsystem tag, emitted right after the
    /// level rather than as an inline field.</summary>
    private const string AreaKey = "area";

    /// <summary>Property holding the message text (the constant template the
    /// <see cref="FileLog"/> facade uses); rendered as the trailing <c>msg=</c>
    /// rather than duplicated as an inline field.</summary>
    private const string MessageKey = "Msg";

    /// <inheritdoc/>
    public void Format(LogEvent logEvent, TextWriter output)
    {
        ArgumentNullException.ThrowIfNull(logEvent);
        ArgumentNullException.ThrowIfNull(output);
        output.Write(Render(logEvent));
        output.Write('\n');
    }

    /// <summary>Render one event as a logfmt line with no trailing newline —
    /// pure, so the exact shape is unit-testable without a sink.</summary>
    /// <param name="e">The event to render.</param>
    /// <returns>The formatted logfmt line.</returns>
    internal static string Render(LogEvent e)
    {
        var sb = new StringBuilder(128);
        sb.Append(e.Timestamp.ToString("yyyy-MM-ddTHH:mm:ss.fffzzz", CultureInfo.InvariantCulture));
        sb.Append(' ').Append(Level(e.Level));

        sb.Append(" area=");
        AppendValue(sb, AreaOf(e));

        // Inline fields (e.g. rid, hits), ordinal-sorted for deterministic
        // output, with the two reserved keys held back for fixed positions.
        foreach (var prop in e.Properties
            .Where(p => !string.Equals(p.Key, AreaKey, StringComparison.Ordinal)
                && !string.Equals(p.Key, MessageKey, StringComparison.Ordinal))
            .OrderBy(p => p.Key, StringComparer.Ordinal))
        {
            sb.Append(' ').Append(prop.Key).Append('=');
            AppendValue(sb, Scalar(prop.Value));
        }

        // Use the raw Msg property value, not RenderMessage: a "{Msg}" template
        // renders a string property *with* surrounding quotes (Serilog's
        // structured form), which our own quoting would then double.
        var message = e.Properties.TryGetValue(MessageKey, out var msg)
            ? Scalar(msg)
            : e.RenderMessage(CultureInfo.InvariantCulture);
        sb.Append(" msg=");
        AppendValue(sb, message);

        if (e.Exception is not null)
        {
            // The whole exception (incl. stack trace) folds onto this one line:
            // its newlines become "\n" so the err= value stays a single record.
            sb.Append(" err=");
            AppendValue(sb, e.Exception.ToString());
        }

        return sb.ToString();
    }

    private static string AreaOf(LogEvent e) =>
        e.Properties.TryGetValue(AreaKey, out var v) ? Scalar(v) : "app";

    private static string Scalar(LogEventPropertyValue v)
    {
        if (v is ScalarValue s)
        {
            return s.Value switch
            {
                null => string.Empty,
                string str => str,
                bool b => b ? "true" : "false",
                IFormattable f => f.ToString(null, CultureInfo.InvariantCulture),
                var other => Convert.ToString(other, CultureInfo.InvariantCulture) ?? string.Empty,
            };
        }

        // Structured values aren't expected (the facade never destructures) but
        // must not throw — render culture-invariantly.
        using var writer = new StringWriter(CultureInfo.InvariantCulture);
        v.Render(writer, formatProvider: CultureInfo.InvariantCulture);
        return writer.ToString();
    }

    private static string Level(LogEventLevel level) => level switch
    {
        LogEventLevel.Verbose => "TRACE",
        LogEventLevel.Debug => "DEBUG",
        LogEventLevel.Information => "INFO ",
        LogEventLevel.Warning => "WARN ",
        LogEventLevel.Error => "ERROR",
        LogEventLevel.Fatal => "FATAL",
        _ => "INFO ",
    };

    /// <summary>Append <paramref name="value"/> as a logfmt value: emitted bare
    /// when safe, else wrapped in <c>"…"</c> with control characters escaped
    /// (the log-injection defence). Capped at <see cref="ValueCap"/> with a
    /// <c>…</c> marker.</summary>
    /// <param name="sb">Destination buffer.</param>
    /// <param name="value">The raw value text.</param>
    internal static void AppendValue(StringBuilder sb, string value)
    {
        var truncated = value.Length > ValueCap;
        if (truncated)
        {
            value = value[..ValueCap];
            if (char.IsHighSurrogate(value[^1]))
            {
                value = value[..^1]; // never split a surrogate pair
            }
        }

        if (!truncated && value.Length != 0 && !ContainsSpecial(value))
        {
            sb.Append(value);
            return;
        }

        sb.Append('"');
        foreach (var ch in value)
        {
            switch (ch)
            {
                case '"': sb.Append("\\\""); break;
                case '\\': sb.Append("\\\\"); break;
                case '\n': sb.Append("\\n"); break;
                case '\r': sb.Append("\\r"); break;
                case '\t': sb.Append("\\t"); break;
                default:
                    if (ch < 0x20)
                    {
                        sb.Append("\\u").Append(((int)ch).ToString("x4", CultureInfo.InvariantCulture));
                    }
                    else
                    {
                        sb.Append(ch);
                    }

                    break;
            }
        }

        if (truncated)
        {
            sb.Append('…');
        }

        sb.Append('"');
    }

    private static bool ContainsSpecial(string value)
    {
        foreach (var ch in value)
        {
            if (ch is ' ' or '=' or '"' or '\\' || ch < 0x20)
            {
                return true;
            }
        }

        return false;
    }
}
