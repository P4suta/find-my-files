namespace FindMyFiles.Engine;

// Data shapes of the engine boundary (the DTO half of IEngineClient.cs;
// ADR-0018). JSON-backed types deserialize with EngineJson.SnakeCase and
// mirror the golden fixtures in contract/golden/ — GoldenCorpusTests pins
// every field against the same files the Rust suite pins.

/// <summary>What <see cref="IEngineClient.SearchAsync"/> returns: the
/// materialized <see cref="ISearchResult"/> the UI pages through, paired with
/// the per-query <see cref="QueryTraceData"/> the engine attached (null when
/// tracing was unavailable, e.g. a serialization failure — the result is
/// still valid).</summary>
/// <param name="Result">The sort-ordered, O(1)-paged result set.</param>
/// <param name="Trace">Stage timings for the F12 perf panel, or null.</param>
public sealed record SearchOutcome(ISearchResult Result, QueryTraceData? Trace);

/// <summary>Stage breakdown of one query (mirrors fmf-core metrics.rs).</summary>
public sealed class QueryTraceData
{
    /// <summary>The query text exactly as the engine parsed it.</summary>
    public string Query { get; set; } = string.Empty;

    /// <summary>Which execution strategy generated the candidates (shown in
    /// the perf panel): e.g. <c>"full-scan"</c>, <c>"pool-scan"</c>,
    /// <c>"suffix"</c>, <c>"perm-walk"</c>.</summary>
    public string Driver { get; set; } = string.Empty;

    /// <summary>Per-volume query-cache outcome: "miss", "refine" (all
    /// volumes narrowed the previous result) or "partial" (mixed).</summary>
    public string Cache { get; set; } = string.Empty;

    /// <summary>µs spent parsing the query text into its AST.</summary>
    public ulong ParseUs { get; set; }

    /// <summary>µs spent compiling the AST into the matcher.</summary>
    public ulong CompileUs { get; set; }

    /// <summary>µs spent on the dir-path memo (path queries only; 0 when warm
    /// or not a path query).</summary>
    public ulong MemoUs { get; set; }

    /// <summary>µs spent scanning candidates across all volumes.</summary>
    public ulong ScanUs { get; set; }

    /// <summary>µs spent materializing matched rows into the result.</summary>
    public ulong MaterializeUs { get; set; }

    /// <summary>µs spent on the multi-volume k-way merge (0 for one
    /// volume).</summary>
    public ulong MergeUs { get; set; }

    /// <summary>End-to-end µs for the whole query; the value fed into the
    /// p50/p99 histogram.</summary>
    public ulong TotalUs { get; set; }

    /// <summary>Entries examined by the scan (the denominator of the match
    /// rate; far larger than <see cref="Hits"/> for a full scan).</summary>
    public ulong EntriesScanned { get; set; }

    /// <summary>Entries skipped by exclusion rules (hidden/system filtering,
    /// excluded paths) before matching.</summary>
    public ulong ExcludedSkipped { get; set; }

    /// <summary>Rows that matched — the resulting <see cref="ISearchResult.Count"/>.</summary>
    public ulong Hits { get; set; }

    /// <summary>How many volume indexes participated in this query.</summary>
    public uint Volumes { get; set; }

    /// <summary>Engine-verified: same query as last time with identical id
    /// lists on every volume — the screen has nothing to change.</summary>
    public bool Unchanged { get; set; }
}

/// <summary>Memory accounting and generation state for one volume index
/// (subset of fmf-core's <c>IndexStats</c> the UI surfaces). Feeds the
/// bytes/entry gate in the F12 panel.</summary>
public sealed class IndexStatsData
{
    /// <summary>Drive label this index covers (e.g. <c>"C:"</c>).</summary>
    public string Volume { get; set; } = string.Empty;

    /// <summary>Total rows including tombstones — the physical slot
    /// count.</summary>
    public ulong Entries { get; set; }

    /// <summary>Rows that still resolve to a live file (the searchable
    /// population).</summary>
    public ulong LiveEntries { get; set; }

    /// <summary>Dead rows awaiting compaction (deleted entries not yet
    /// reclaimed); <c>Entries - LiveEntries</c>.</summary>
    public ulong Tombstones { get; set; }

    /// <summary>Resident bytes of this index in the engine process (all
    /// columns + derived query caches) — the numerator of the bytes/entry
    /// gate.</summary>
    public ulong TotalBytes { get; set; }

    /// <summary><c>TotalBytes / Entries</c>; the headline RAM-efficiency
    /// figure measured against the ≤110 B/file target.</summary>
    public double BytesPerEntry { get; set; }

    /// <summary>Bumps on every content change (USN apply, rescan). A stable
    /// value means an <see cref="QueryTraceData.Unchanged"/> re-query is
    /// possible; the structural generation (not exposed here) is what
    /// invalidates a result with <see cref="StaleResultException"/>.</summary>
    public ulong ContentGeneration { get; set; }
}

/// <summary>One applied USN journal batch — how a burst of filesystem changes
/// landed in the index (mirrors fmf-core's <c>UsnTrace</c>). Drives the
/// change-reflection latency shown in the perf panel.</summary>
public sealed class UsnTraceData
{
    /// <summary>Drive label the batch was applied to.</summary>
    public string Volume { get; set; } = string.Empty;

    /// <summary>Raw USN records in the batch (before coalescing).</summary>
    public ulong Records { get; set; }

    /// <summary>Entries created or updated in the index.</summary>
    public ulong Upserted { get; set; }

    /// <summary>Entries tombstoned (deletes/renames-away).</summary>
    public ulong Deleted { get; set; }

    /// <summary>Entries whose size/mtime were refreshed via a stat
    /// fetch.</summary>
    public ulong StatUpdated { get; set; }

    /// <summary>Stat fetches that failed (the entry keeps its prior
    /// size/mtime); also counted in <see cref="CountersData.StatFetchFailures"/>.</summary>
    public ulong StatFailures { get; set; }

    /// <summary>µs to apply the whole batch to the index.</summary>
    public ulong ApplyUs { get; set; }
}

/// <summary>One entry from the engine's diagnostic ring (WARN+ events and
/// panics; mirrors fmf-core's <c>ErrorEvent</c>). Pulled on demand after an
/// <see cref="IEngineClient.EngineErrorOccurred"/> signal and listed in the
/// F12 panel.</summary>
public sealed class ErrorEventData
{
    /// <summary>Monotonic emit sequence — orders events and lets the UI
    /// detect ones it has already shown.</summary>
    public ulong Seq { get; set; }

    /// <summary>Engine uptime in ms when the event fired ("when").</summary>
    public ulong UptimeMs { get; set; }

    /// <summary>Level as a lowercase string: <c>"warn"</c>, <c>"error"</c> or
    /// <c>"panic"</c> (the same 1/2/3 the FFI event carries numerically).</summary>
    public string Severity { get; set; } = string.Empty; // warn|error|panic

    /// <summary>Originating <c>tracing</c> target (module path) — the "where"
    /// of the event.</summary>
    public string Area { get; set; } = string.Empty;

    /// <summary>Drive label the event is attributed to, or null when it is
    /// not volume-scoped.</summary>
    public string? Volume { get; set; }

    /// <summary>Human-readable description of what happened.</summary>
    public string Message { get; set; } = string.Empty;
}

// CountersData is generated (Generated/EngineContract.g.cs) from the
// contract's counter-name registry — adding an engine counter radiates to
// C# via `just contract-gen` (ADR-0018). The handwritten copy that lived
// here was missing four counters; the generated one cannot be.

/// <summary>Client-side pipe transport metrics. Null for in-proc clients;
/// the pipe client fills it on every <see cref="IEngineClient.GetStatsAsync"/>.</summary>
public sealed class TransportStatsData
{
    /// <summary>Current <see cref="EngineConnectionState"/> rendered as text
    /// (e.g. <c>"Connected"</c>, <c>"Reconnecting"</c>).</summary>
    public string State { get; set; } = string.Empty;

    /// <summary>How many times the supervisor has re-established the pipe
    /// since process start — a churn indicator.</summary>
    public long Reconnects { get; set; }

    /// <summary>EWMA of page-fetch round-trip latency in µs (the wire cost a
    /// page read adds on top of the engine's own time).</summary>
    public double PageRttEwmaUs { get; set; }

    /// <summary>PID of the fmf-service process on the other end (from the
    /// Hello handshake); shown so users can find it in Task Manager.</summary>
    public uint ServerPid { get; set; }

    /// <summary>ABI version the server reported at handshake — must match the
    /// client's <c>EngineContract.AbiVersion</c>.</summary>
    public uint AbiVersion { get; set; }
}

/// <summary>The whole observability snapshot behind the F12 perf panel —
/// what <see cref="IEngineClient.GetStatsAsync"/> returns (the UI subset of
/// fmf-core's <c>MetricsSnapshot</c>).</summary>
public sealed class EngineStatsData
{
    /// <summary>Recent query traces, oldest first (a bounded ring on the
    /// engine side).</summary>
    public List<QueryTraceData> RecentQueries { get; set; } = [];

    /// <summary>Median query latency in µs across the histogram's
    /// lifetime.</summary>
    public ulong P50Us { get; set; }

    /// <summary>99th-percentile query latency in µs — the figure measured
    /// against the ≤50 ms search gate.</summary>
    public ulong P99Us { get; set; }

    /// <summary>Recently applied USN batches, oldest first.</summary>
    public List<UsnTraceData> RecentUsn { get; set; } = [];

    /// <summary>Per-volume index accounting, one entry per indexed
    /// volume.</summary>
    public List<IndexStatsData> Indexes { get; set; } = [];

    /// <summary>Degradation counters; nonzero values flag silent
    /// fallbacks.</summary>
    public CountersData Counters { get; set; } = new();

    /// <summary>WARN+ diagnostics and panics from the engine's ring, oldest
    /// first.</summary>
    public List<ErrorEventData> RecentErrors { get; set; } = [];

    /// <summary>Pipe transport metrics, or null for in-proc clients (Ffi/Fake)
    /// where there is no wire.</summary>
    public TransportStatsData? Transport { get; set; }
}

/// <summary>Result sort key (wire values of fmf-core's <c>SortKey</c>).</summary>
public enum FmfSort
{
    /// <summary>Sort by file name.</summary>
    Name = 0,

    /// <summary>Sort by file size in bytes.</summary>
    Size = 1,

    /// <summary>Sort by modification time.</summary>
    Mtime = 2,
}

/// <summary>Case-matching mode (wire values of fmf-core's <c>CaseMode</c>).</summary>
public enum FmfCase
{
    /// <summary>Case-insensitive unless the query contains an uppercase
    /// letter, in which case it becomes case-sensitive.</summary>
    Smart = 0,

    /// <summary>Always case-insensitive.</summary>
    Insensitive = 1,

    /// <summary>Always case-sensitive.</summary>
    Sensitive = 2,
}

/// <summary>Which haystack a whole-query regex runs against (wire values of
/// fmf-core's <c>RegexScope</c>; the <c>regex_mode</c> bit1).</summary>
public enum RegexScope
{
    /// <summary>Match the file name.</summary>
    Name = 0,

    /// <summary>Match the full path.</summary>
    Path = 1,
}

/// <summary>The knobs that shape a search, passed to
/// <see cref="IEngineClient.SearchAsync"/> (the C# face of fmf-core's
/// <c>FmfQueryOptions</c>).</summary>
/// <param name="Sort">Which key to order results by.</param>
/// <param name="Descending">True for descending order; false for
/// ascending.</param>
/// <param name="Case">How query case is matched against names.</param>
/// <param name="IncludeHiddenSystem">When true, hidden/system entries are
/// included; they are excluded by default.</param>
/// <param name="RegexMode">When true, the whole query text is one regular
/// expression (the <c>regex:</c> per-term syntax still works regardless).</param>
/// <param name="Scope">Which haystack the whole-query regex matches against
/// (ignored unless <paramref name="RegexMode"/>).</param>
public sealed record SearchOptions(
    FmfSort Sort,
    bool Descending,
    FmfCase Case,
    bool IncludeHiddenSystem = false,
    bool RegexMode = false,
    RegexScope Scope = RegexScope.Name)
{
    /// <summary>The app's default search: by <see cref="FmfSort.Name"/>,
    /// ascending, <see cref="FmfCase.Smart"/> case, hidden/system
    /// excluded, regex mode off.</summary>
    public static readonly SearchOptions Default = new(FmfSort.Name, false, FmfCase.Smart);

    /// <summary>The packed <c>FmfQueryOptions.regex_mode</c> wire value:
    /// bit0 = whole-query regex on, bit1 = scope (0 name / 1 path).</summary>
    public uint RegexModeBits =>
        (RegexMode ? 1u : 0u) | (Scope == RegexScope.Path ? 2u : 0u);
}

/// <summary>Lifecycle of a volume's index (wire values of fmf-core's
/// <c>VolumeState</c>), reported via <see cref="IEngineClient.VolumeUpdated"/>.</summary>
public enum VolumeState
{
    /// <summary>Initial scan in progress; results are incomplete.</summary>
    Scanning = 0,

    /// <summary>Index is complete and searchable.</summary>
    Ready = 1,

    /// <summary>A rescan is rebuilding an already-ready index (e.g. after a
    /// USN gap); the prior result stays usable.</summary>
    Rescanning = 2,

    /// <summary>Indexing failed (access denied, unsupported filesystem); this
    /// volume contributes no results.</summary>
    Failed = 3,
}

/// <summary>A volume's current index status — the payload of an
/// <see cref="IEngineClient.VolumeUpdated"/> event and of
/// <see cref="IEngineClient.GetStatusAsync"/>.</summary>
/// <param name="Label">Drive label (e.g. <c>"C:"</c>).</param>
/// <param name="State">Where the index is in its lifecycle.</param>
/// <param name="Entries">Indexed entry count so far (grows while
/// <see cref="VolumeState.Scanning"/>).</param>
public sealed record VolumeStatus(string Label, VolumeState State, ulong Entries);

/// <summary>One result row decoded from a page (the C# face of fmf-core's
/// 48-byte <c>FmfRow</c> plus its WTF-8 name/path strings). Immutable; the
/// UI's <c>ResultRow</c> view-model is filled from it.</summary>
/// <param name="EntryRef">Engine-internal stable handle for the entry within
/// its volume index (the identity used for refine/<c>unchanged</c>
/// comparisons) — not the NTFS reference.</param>
/// <param name="Frn">NTFS File Reference Number (record number in the low 48
/// bits, sequence in the high 16) — the identity to correlate with USN
/// records and the filesystem.</param>
/// <param name="Size">File size in bytes (0 for directories).</param>
/// <param name="Mtime">Last-modified time as a Windows <c>FILETIME</c>
/// (100 ns ticks since 1601-01-01 UTC); 0 means unknown/unset and renders as
/// an empty timestamp.</param>
/// <param name="Flags">Bit field of NTFS attributes; bit 0 is the directory
/// flag (see <see cref="IsDirectory"/>).</param>
/// <param name="Name">Leaf file or directory name.</param>
/// <param name="ParentPath">Containing directory path including its trailing
/// separator (e.g. <c>"C:\"</c>), so <see cref="FullPath"/> is a plain
/// concatenation.</param>
public sealed record RowData(
    ulong EntryRef,
    ulong Frn,
    ulong Size,
    long Mtime,
    uint Flags,
    string Name,
    string ParentPath)
{
    /// <summary>True when this row is a directory (bit 0 of
    /// <see cref="Flags"/>).</summary>
    public bool IsDirectory => (Flags & 1) != 0;

    /// <summary>The full path, <see cref="ParentPath"/> (which already ends in
    /// a separator) concatenated with <see cref="Name"/>.</summary>
    public string FullPath => ParentPath + Name;
}
