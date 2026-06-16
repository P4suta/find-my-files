namespace FindMyFiles.Engine;

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
