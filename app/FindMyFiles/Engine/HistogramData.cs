namespace FindMyFiles.Engine;

/// <summary>Log2-bucketed latency histogram (mirrors fmf-core's
/// <c>Histogram</c>): bucket <c>i</c> counts query latencies in
/// <c>[2^i, 2^(i+1))</c> µs. The standard HdrHistogram-style distribution the
/// F12 panel renders behind the p50/p90/p99/p99.9 line.</summary>
public sealed class HistogramData
{
    /// <summary>Per-bucket counts (length 32); bucket <c>i</c> covers
    /// <c>[2^i, 2^(i+1))</c> µs.</summary>
    public List<ulong> Buckets { get; set; } = [];

    /// <summary>Total number of recorded queries.</summary>
    public ulong Count { get; set; }

    /// <summary>Sum of all recorded latencies, in µs.</summary>
    public ulong SumUs { get; set; }

    /// <summary>Largest recorded latency, in µs.</summary>
    public ulong MaxUs { get; set; }
}
