namespace FindMyFiles.Engine;

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
