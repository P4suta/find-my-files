namespace FindMyFiles.Engine;

/// <summary>The whole observability snapshot behind the F12 perf panel —
/// what <see cref="IEngineClient.GetStatsAsync"/> returns (the UI subset of
/// fmf-core's <c>MetricsSnapshot</c>).</summary>
public sealed class EngineStatsData
{
    /// <summary>Recent query traces, oldest first (a bounded ring on the
    /// engine side).</summary>
    public List<QueryTraceData> RecentQueries { get; set; } = [];

    /// <summary>Median (p50) query latency in µs across the histogram's
    /// lifetime.</summary>
    public ulong P50Us { get; set; }

    /// <summary>90th-percentile query latency in µs.</summary>
    public ulong P90Us { get; set; }

    /// <summary>99th-percentile query latency in µs — the figure measured
    /// against the ≤50 ms search gate.</summary>
    public ulong P99Us { get; set; }

    /// <summary>99.9th-percentile query latency in µs (tail latency).</summary>
    public ulong P999Us { get; set; }

    /// <summary>Full latency distribution behind the percentile line.</summary>
    public HistogramData QueryHistogram { get; set; } = new();

    /// <summary>Recently applied USN batches, oldest first.</summary>
    public List<UsnTraceData> RecentUsn { get; set; } = [];

    /// <summary>Index-established (scan/snapshot restore) events, oldest
    /// first — startup performance and its memory peak.</summary>
    public List<ScanTraceData> Scans { get; set; } = [];

    /// <summary>Per-volume index accounting, one entry per indexed
    /// volume.</summary>
    public List<IndexStatsData> Indexes { get; set; } = [];

    /// <summary>Working Set of the host process in bytes — the engine's live
    /// footprint (the <c>Working Set</c> figure Task Manager reports). In pipe
    /// mode this is the <b>service</b> process; under <c>--engine=inproc</c> it
    /// is the app process.</summary>
    public ulong CurrentWsBytes { get; set; }

    /// <summary>Private (committed) bytes of the host process — the
    /// <c>Private Bytes</c> figure Task Manager reports. Same host-process
    /// semantics as <see cref="CurrentWsBytes"/>.</summary>
    public ulong CurrentPrivateBytes { get; set; }

    /// <summary>Degradation counters; nonzero values flag silent
    /// fallbacks.</summary>
    public CountersData Counters { get; set; } = new();

    /// <summary>WARN+ diagnostics and panics from the engine's ring, oldest
    /// first.</summary>
    public List<ErrorEventData> RecentErrors { get; set; } = [];

    /// <summary>Pipe transport metrics, or null for in-proc clients (Ffi/Fake)
    /// where there is no wire.</summary>
    public TransportStatsData? Transport { get; set; }

    /// <summary>fmf-service runtime info (uptime/connections/version), or null
    /// for in-proc clients (Ffi/Fake) where there is no separate service.</summary>
    public ServiceInfoData? Service { get; set; }
}
