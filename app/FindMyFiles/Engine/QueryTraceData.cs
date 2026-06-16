namespace FindMyFiles.Engine;

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
