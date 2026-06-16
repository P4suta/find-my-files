namespace FindMyFiles.Engine;

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
