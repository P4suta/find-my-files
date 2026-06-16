namespace FindMyFiles.Engine;

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
