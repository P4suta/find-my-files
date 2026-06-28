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

    /// <summary>The standard latency-percentile line — p50/p90/p99/p99.9, the
    /// figures monitoring tools report (e.g.
    /// <c>"p50 0.82 ms · p90 1.90 ms · p99 4.21 ms · p99.9 9.00 ms"</c>).</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The percentile line, or empty when <paramref name="s"/> is null.</returns>
    public static string Percentiles(EngineStatsData? s) =>
        s is null
            ? string.Empty
            : $"p50 {Ms(s.P50Us)} · p90 {Ms(s.P90Us)} · p99 {Ms(s.P99Us)} · p99.9 {Ms(s.P999Us)}";

    /// <summary>The host process's live memory footprint, using the standard
    /// Windows names (<c>"Private Bytes 142 MB · Working Set 138 MB"</c>) —
    /// the same figures Task Manager / Process Explorer report. In pipe mode
    /// this is the service process; under <c>--engine=inproc</c> the app.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The memory line, or empty when <paramref name="s"/> is null.</returns>
    public static string ProcessMemory(EngineStatsData? s) =>
        s is null
            ? string.Empty
            : $"Private Bytes {Mb(s.CurrentPrivateBytes)} · Working Set {Mb(s.CurrentWsBytes)}";

    /// <summary>Total resident index bytes summed across all volumes (the
    /// engine's own accounting), as whole MB.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The index total, or empty when <paramref name="s"/> is null.</returns>
    public static string IndexTotal(EngineStatsData? s) =>
        s is null ? string.Empty : Mb(IndexBytes(s));

    /// <summary>Process working set minus the indexes' resident bytes — the
    /// non-index overhead (engine + runtime), as whole MB. Clamped at 0.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The overhead, or empty when <paramref name="s"/> is null.</returns>
    public static string Overhead(EngineStatsData? s)
    {
        if (s is null)
        {
            return string.Empty;
        }

        var idx = IndexBytes(s);
        return Mb(s.CurrentWsBytes > idx ? s.CurrentWsBytes - idx : 0);
    }

    /// <summary>Per-column byte breakdown for one volume index (where the RAM
    /// goes), with the spool garbage ratio. Member-wise (not object) so it
    /// binds from an <c>IndexStatsData</c> DataTemplate, like
    /// <see cref="Usn"/>.</summary>
    /// <param name="namePool">Original-name spool bytes.</param>
    /// <param name="lowerPool">Case-folded name spool bytes.</param>
    /// <param name="offsets">Name-offset table bytes.</param>
    /// <param name="parent">Parent-pointer column bytes.</param>
    /// <param name="perm">Name-sort permutation bytes.</param>
    /// <param name="frnMap">FRN→row lookup map bytes.</param>
    /// <param name="deadName">Reclaimable dead-name spool bytes.</param>
    /// <param name="garbageRatio">Dead-name fraction of the spools (0..1).</param>
    /// <returns>The breakdown line.</returns>
    public static string IndexColumns(
        ulong namePool,
        ulong lowerPool,
        ulong offsets,
        ulong parent,
        ulong perm,
        ulong frnMap,
        ulong deadName,
        double garbageRatio) =>
        $"name {Mb(namePool)} · lower {Mb(lowerPool)} · offsets {Mb(offsets)} · " +
        $"parent {Mb(parent)} · perm {Mb(perm)} · frn-map {Mb(frnMap)} · " +
        $"dead {Mb(deadName)} ({Percent(garbageRatio)})";

    /// <summary>One index-established (scan/restore) event as a compact line
    /// (e.g. <c>"C: snapshot 1.2 s · 850 MB/s · parse 34ms build 36ms sort 37ms · peak 410 MB · 1,234,567件"</c>).
    /// Member-wise so it binds from a <c>ScanTraceData</c> DataTemplate.</summary>
    /// <param name="volume">Drive label.</param>
    /// <param name="source">How the index was established (scan/snapshot).</param>
    /// <param name="mbPerS">Read throughput in MB/s.</param>
    /// <param name="parseMs">ms spent parsing MFT records.</param>
    /// <param name="buildMs">ms spent building the index.</param>
    /// <param name="sortMs">ms spent sorting the permutation.</param>
    /// <param name="totalMs">End-to-end ms.</param>
    /// <param name="entries">Entry count once established.</param>
    /// <param name="peakWs">Peak working set during the establish, in bytes.</param>
    /// <returns>The scan line.</returns>
    public static string Scan(
        string volume,
        string source,
        double mbPerS,
        ulong parseMs,
        ulong buildMs,
        ulong sortMs,
        ulong totalMs,
        ulong entries,
        ulong peakWs) =>
        $"{volume} {source} {Secs(totalMs)} · {mbPerS.ToString("F0", Inv)} MB/s · " +
        $"parse {parseMs}ms build {buildMs}ms sort {sortMs}ms · " +
        $"peak {Mb(peakWs)} · {Count(entries)}件";

    /// <summary>The service runtime line (pipe only): uptime, active
    /// connections and version (e.g. <c>"uptime 2h13m · 接続 1 · v0.1.0"</c>).
    /// Empty for in-proc clients where there is no separate service.</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns>The service line, or empty when there is no service.</returns>
    public static string Service(EngineStatsData? s) =>
        s?.Service is { } svc
            ? $"uptime {Uptime(svc.UptimeMs)} · 接続 {svc.Connections} · v{svc.Version}"
            : string.Empty;

    /// <summary>Visible only when a service is on the other end (pipe clients).</summary>
    /// <param name="s">The stats snapshot, or null before the first poll.</param>
    /// <returns><c>Visible</c> when there is service info, otherwise <c>Collapsed</c>.</returns>
    public static Visibility ServiceVis(EngineStatsData? s) =>
        s?.Service is not null ? Visibility.Visible : Visibility.Collapsed;

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

    private static ulong IndexBytes(EngineStatsData s)
    {
        ulong total = 0;
        foreach (var ix in s.Indexes)
        {
            total += ix.TotalBytes;
        }

        return total;
    }

    /// <summary>A 0..1 ratio as a one-decimal percentage (e.g. <c>"1.5%"</c>).</summary>
    private static string Percent(double ratio) => (ratio * 100.0).ToString("F1", Inv) + "%";

    /// <summary>Milliseconds as seconds with one decimal once past a second
    /// (e.g. <c>"1.2 s"</c>), otherwise whole milliseconds (e.g. <c>"820ms"</c>).</summary>
    private static string Secs(ulong ms) =>
        ms >= 1000 ? (ms / 1000.0).ToString("F1", Inv) + " s" : ms.ToString(Inv) + "ms";

    /// <summary>Milliseconds as a coarse uptime: <c>"2h13m"</c> / <c>"13m"</c> /
    /// <c>"45s"</c>.</summary>
    private static string Uptime(ulong ms)
    {
        var totalSecs = ms / 1000;
        var hours = totalSecs / 3600;
        var mins = (totalSecs % 3600) / 60;
        if (hours > 0)
        {
            return $"{hours}h{mins:D2}m";
        }

        return mins > 0 ? $"{mins}m" : $"{totalSecs}s";
    }

    private static readonly PropertyInfo[] CounterProps =
        [.. typeof(CountersData).GetProperties().Where(p => p.PropertyType == typeof(ulong))];

    private static string FirstLine(string s)
    {
        var i = s.IndexOf('\n', StringComparison.Ordinal);
        var line = i < 0 ? s : s[..i];
        return line.Length > 120 ? line[..120] + "…" : line;
    }
}
