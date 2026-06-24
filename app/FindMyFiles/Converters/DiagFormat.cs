using System.Globalization;
using System.Reflection;
using FindMyFiles.Engine;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Converters;

/// <summary>
/// Pure x:Bind formatters for the F12 diagnostics panel. These produce the
/// dense numeric/diagnostic strings the panel shows — not localized prose —
/// so everything is formatted with <see cref="CultureInfo.InvariantCulture"/>:
/// stable grouping for values pasted into bug reports, and deterministic unit
/// tests regardless of the build agent's locale. Moved out of the panel's
/// imperative code-behind so the formatting is testable (the chrome — stage
/// bar / sparkline — stays in code-behind). UI thread only via x:Bind.
/// </summary>
public static class DiagFormat
{
    private static readonly CultureInfo Inv = CultureInfo.InvariantCulture;

    /// <summary>Group-separated integer (e.g. <c>"812,003"</c>).</summary>
    /// <param name="n">The value to format.</param>
    /// <returns>The invariant <c>N0</c> rendering.</returns>
    public static string Count(ulong n) => n.ToString("N0", Inv);

    /// <summary>Whole mebibytes with a unit (e.g. <c>"91 MB"</c>).</summary>
    /// <param name="bytes">Resident byte count.</param>
    /// <returns>The value in MiB, rounded to whole units.</returns>
    public static string Mb(ulong bytes) =>
        ((double)bytes / (1024.0 * 1024.0)).ToString("F0", Inv) + " MB";

    /// <summary>Microseconds rendered as milliseconds (e.g. <c>"2.34 ms"</c>).</summary>
    /// <param name="micros">Duration in µs.</param>
    /// <returns>The duration in ms to two decimals.</returns>
    public static string Ms(ulong micros) => (micros / 1000.0).ToString("F2", Inv) + " ms";

    /// <summary>Bytes-per-entry with a unit (e.g. <c>"91 B/件"</c>) — the RAM
    /// efficiency figure measured against the ≤110 B/file target.</summary>
    /// <param name="bytesPerEntry">Resident bytes divided by entry count.</param>
    /// <returns>The whole-byte rendering with the per-entry unit.</returns>
    public static string BytesPer(double bytesPerEntry) =>
        bytesPerEntry.ToString("F0", Inv) + " B/件";

    /// <summary>Whole microseconds with a unit (e.g. <c>"60 µs"</c>).</summary>
    /// <param name="micros">Duration in µs.</param>
    /// <returns>The rounded µs rendering.</returns>
    public static string Micros(double micros) => micros.ToString("F0", Inv) + " µs";

    /// <summary>Content-generation label (e.g. <c>"gen 7"</c>).</summary>
    /// <param name="generation">The volume's content generation counter.</param>
    /// <returns>The labelled generation.</returns>
    public static string Gen(ulong generation) => "gen " + generation.ToString(Inv);

    /// <summary>The most recent query text shown on its own line above the stat
    /// tiles (<c>"(all)"</c> for the empty query).</summary>
    /// <param name="t">The last query trace, or null when none was emitted.</param>
    /// <returns>The query text, or empty when <paramref name="t"/> is null.</returns>
    public static string Query(QueryTraceData? t) =>
        t is null ? string.Empty : (string.IsNullOrEmpty(t.Query) ? "(all)" : t.Query);

    /// <summary>End-to-end time as the "時間" stat-tile value (e.g. <c>"6.05 ms"</c>).</summary>
    /// <param name="t">The last query trace, or null when none was emitted.</param>
    /// <returns>The total time, or empty when <paramref name="t"/> is null.</returns>
    public static string TotalMs(QueryTraceData? t) => t is null ? string.Empty : Ms(t.TotalUs);

    /// <summary>Match count as the "ヒット" stat-tile value (e.g. <c>"1,234"</c>).</summary>
    /// <param name="t">The last query trace, or null when none was emitted.</param>
    /// <returns>The hit count, or empty when <paramref name="t"/> is null.</returns>
    public static string Hits(QueryTraceData? t) => t is null ? string.Empty : Count(t.Hits);

    /// <summary>Entries examined as the "走査" stat-tile value (e.g. <c>"100,000"</c>).</summary>
    /// <param name="t">The last query trace, or null when none was emitted.</param>
    /// <returns>The scanned count, or empty when <paramref name="t"/> is null.</returns>
    public static string Scanned(QueryTraceData? t) => t is null ? string.Empty : Count(t.EntriesScanned);

    /// <summary>Execution strategy as the "方式" stat-tile value (e.g. <c>"suffix"</c>).</summary>
    /// <param name="t">The last query trace, or null when none was emitted.</param>
    /// <returns>The driver label, or empty when <paramref name="t"/> is null.</returns>
    public static string Driver(QueryTraceData? t) => t is null ? string.Empty : t.Driver;

    /// <summary>The p50/p99 latency line (e.g. <c>"p50 0.82 ms · p99 4.21 ms"</c>).</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The percentile line, or empty when <paramref name="s"/> is null.</returns>
    public static string Percentiles(EngineStatsData? s) =>
        s is null ? string.Empty : $"p50 {Ms(s.P50Us)} · p99 {Ms(s.P99Us)}";

    /// <summary>The pipe-transport detail line. Empty for in-proc/fake clients
    /// (no wire), where the panel shows only the engine-mode label above it.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The transport line, or empty when there is no transport.</returns>
    public static string Transport(EngineStatsData? s) =>
        s?.Transport is { } tr
            ? $"{tr.State} · reconnects {tr.Reconnects} · RTT {Micros(tr.PageRttEwmaUs)} · pid {tr.ServerPid}"
            : string.Empty;

    /// <summary>One recently-applied USN batch as a compact change-feed line.</summary>
    /// <param name="volume">Drive label the batch applied to.</param>
    /// <param name="records">Raw USN records in the batch.</param>
    /// <param name="upserted">Entries created or updated.</param>
    /// <param name="deleted">Entries tombstoned.</param>
    /// <param name="statUpdated">Entries whose size/mtime were refreshed.</param>
    /// <param name="applyUs">µs to apply the whole batch.</param>
    /// <returns>The formatted batch line.</returns>
    public static string Usn(
        string volume, ulong records, ulong upserted, ulong deleted, ulong statUpdated, ulong applyUs) =>
        $"{volume} {Count(records)}rec → +{Count(upserted)} -{Count(deleted)} ~{Count(statUpdated)} ({Micros(applyUs)})";

    /// <summary>One engine diagnostic-ring event as a single line.</summary>
    /// <param name="uptimeMs">Engine uptime in ms when the event fired.</param>
    /// <param name="severity">Lowercase level (<c>warn</c>/<c>error</c>/<c>panic</c>).</param>
    /// <param name="area">Originating module/target.</param>
    /// <param name="volume">Drive label, or null when not volume-scoped.</param>
    /// <param name="message">Human-readable description (first line only).</param>
    /// <returns>The formatted event line.</returns>
    public static string Error(ulong uptimeMs, string severity, string area, string? volume, string message)
    {
        var vol = string.IsNullOrEmpty(volume) ? string.Empty : $" ({volume})";
        return $"[{uptimeMs / 1000}s] {severity.ToUpperInvariant()} {area}{vol}: {FirstLine(message)}";
    }

    /// <summary>Nonzero degradation counters as a single line, or empty when all
    /// are zero. Reflects over the generated <see cref="CountersData"/> so a
    /// counter added to the contract registry surfaces here with no UI edits
    /// (ADR-0018) — the regression this guards against.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The degradations line, or empty when clean/null.</returns>
    public static string Counters(EngineStatsData? s)
    {
        if (s is null)
        {
            return string.Empty;
        }

        var c = s.Counters;
        var nonzero = CounterProps
            .Select(p => (
                Name: System.Text.Json.JsonNamingPolicy.SnakeCaseLower.ConvertName(p.Name),
                V: (ulong)p.GetValue(c)!))
            .Where(x => x.V > 0)
            .ToList();
        return nonzero.Count == 0
            ? string.Empty
            : "劣化: " + string.Join("  ", nonzero.Select(x => $"{x.Name}={Count(x.V)}"));
    }

    /// <summary>Visible only when there are no degradation counters and no recent
    /// errors — gates the green "healthy" marker.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns><c>Visible</c> when clean, otherwise <c>Collapsed</c>.</returns>
    public static Visibility HealthyVis(EngineStatsData? s) =>
        s is not null && !AnyDegraded(s) && s.RecentErrors.Count == 0
            ? Visibility.Visible
            : Visibility.Collapsed;

    /// <summary>Visible only when at least one degradation counter is nonzero.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns><c>Visible</c> when degraded, otherwise <c>Collapsed</c>.</returns>
    public static Visibility CountersVis(EngineStatsData? s) =>
        s is not null && AnyDegraded(s) ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Visible only when the engine reported recent WARN+ events.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns><c>Visible</c> when there are errors, otherwise <c>Collapsed</c>.</returns>
    public static Visibility HasErrorsVis(EngineStatsData? s) =>
        s is { RecentErrors.Count: > 0 } ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>The InfoBar severity for the health section: <c>Error</c> when the
    /// engine reported recent WARN+ events (highest priority), <c>Warning</c> when
    /// any degradation counter is nonzero, otherwise <c>Success</c>. Returns
    /// <c>Informational</c> before the first poll (<paramref name="s"/> is null) so
    /// the bar stays neutral instead of falsely reading "healthy".</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The severity reflecting the worst current condition.</returns>
    public static InfoBarSeverity HealthSeverity(EngineStatsData? s)
    {
        if (s is null)
        {
            return InfoBarSeverity.Informational;
        }

        if (s.RecentErrors.Count > 0)
        {
            return InfoBarSeverity.Error;
        }

        return AnyDegraded(s) ? InfoBarSeverity.Warning : InfoBarSeverity.Success;
    }

    private static bool AnyDegraded(EngineStatsData s) =>
        CounterProps.Any(p => (ulong)p.GetValue(s.Counters)! > 0);

    private static readonly PropertyInfo[] CounterProps =
        [.. typeof(CountersData).GetProperties().Where(p => p.PropertyType == typeof(ulong))];

    private static string FirstLine(string s)
    {
        var i = s.IndexOf('\n', StringComparison.Ordinal);
        var line = i < 0 ? s : s[..i];
        return line.Length > 120 ? line[..120] + "…" : line;
    }
}
