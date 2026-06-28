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

    // ── Per-column byte breakdown (where the RAM goes) ──────────────────────
    // The columns sum (with the derived cache) to TotalBytes; surfacing them
    // lets the F12 panel show which structure dominates the footprint.

    /// <summary>Bytes held by the original-name spool (the cased file names).</summary>
    public ulong NamePoolBytes { get; set; }

    /// <summary>Bytes held by the case-folded name spool (used for matching).</summary>
    public ulong LowerPoolBytes { get; set; }

    /// <summary>Bytes of the name-offset table into the spools.</summary>
    public ulong OffsetsBytes { get; set; }

    /// <summary>Bytes of the parent-pointer column (the directory tree).</summary>
    public ulong ParentBytes { get; set; }

    /// <summary>Bytes of the file-size column (u32 + overflow side table).</summary>
    public ulong SizeBytes { get; set; }

    /// <summary>Bytes of the modification-time column.</summary>
    public ulong MtimeBytes { get; set; }

    /// <summary>Bytes of the file-reference-number (FRN) column.</summary>
    public ulong FrnBytes { get; set; }

    /// <summary>Bytes of the per-entry flag column.</summary>
    public ulong FlagBytes { get; set; }

    /// <summary>Bytes of the name-sort permutation (the ordered view).</summary>
    public ulong PermutationsBytes { get; set; }

    /// <summary>Bytes of the FRN→row lookup map (USN apply uses it).</summary>
    public ulong FrnMapBytes { get; set; }

    /// <summary>Spool bytes occupied by superseded names awaiting compaction.</summary>
    public ulong DeadNameBytes { get; set; }

    /// <summary><c>DeadNameBytes / (NamePoolBytes + LowerPoolBytes)</c> — how
    /// much of the spools is reclaimable garbage (compaction trigger).</summary>
    public double PoolGarbageRatio { get; set; }

    /// <summary>Bytes of query-layer derived caches (e.g. directory-path
    /// memos) attributed to this volume.</summary>
    public ulong DerivedCacheBytes { get; set; }

    /// <summary>Bumps on every content change (USN apply, rescan). A stable
    /// value means an <see cref="QueryTraceData.Unchanged"/> re-query is
    /// possible.</summary>
    public ulong ContentGeneration { get; set; }

    /// <summary>Bumps on every structural change (add/delete/rename) — what
    /// invalidates a held result with <see cref="StaleResultException"/>.</summary>
    public ulong StructuralGeneration { get; set; }
}
