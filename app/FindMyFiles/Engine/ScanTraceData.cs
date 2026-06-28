namespace FindMyFiles.Engine;

/// <summary>One index-established event — a full MFT scan or a snapshot
/// restore (mirrors fmf-core's <c>ScanTrace</c>). Surfaces in the F12 panel so
/// a slow startup can be attributed to its phase (read vs parse vs build vs
/// sort) and its memory peak.</summary>
public sealed class ScanTraceData
{
    /// <summary>Drive label this index covers (e.g. <c>"C:"</c>).</summary>
    public string Volume { get; set; } = string.Empty;

    /// <summary>How the index was established: <c>"scan"</c> (full MFT read) or
    /// <c>"snapshot"</c> (restore from disk).</summary>
    public string Source { get; set; } = string.Empty;

    /// <summary>Bytes read from the MFT or snapshot file.</summary>
    public ulong ReadBytes { get; set; }

    /// <summary>ms spent reading.</summary>
    public ulong ReadMs { get; set; }

    /// <summary>Read throughput in MB/s (<see cref="ReadBytes"/> over
    /// <see cref="ReadMs"/>).</summary>
    public double MbPerS { get; set; }

    /// <summary>ms spent parsing MFT records.</summary>
    public ulong ParseMs { get; set; }

    /// <summary>ms spent resolving <c>$ATTRIBUTE_LIST</c> deferred names.</summary>
    public ulong DeferredMs { get; set; }

    /// <summary>ms spent building the index columns.</summary>
    public ulong BuildMs { get; set; }

    /// <summary>ms spent sorting the name permutation.</summary>
    public ulong SortMs { get; set; }

    /// <summary>End-to-end ms for the whole establish.</summary>
    public ulong TotalMs { get; set; }

    /// <summary>Entry count once the index was established.</summary>
    public ulong Entries { get; set; }

    /// <summary>Peak working set in bytes during the establish — includes the
    /// transient scan/build buffers (higher than the steady-state footprint).</summary>
    public ulong PeakWsBytes { get; set; }
}
