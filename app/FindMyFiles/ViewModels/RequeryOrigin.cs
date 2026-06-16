namespace FindMyFiles.ViewModels;

/// <summary>
/// Why a requery ran. Reset origins land the user at the top of the list;
/// position-preserving origins restore the previous viewport
/// (docs/ARCHITECTURE.md「再クエリの2系統」).
/// </summary>
public enum RequeryOrigin
{
    /// <summary>First query of the session — reset (top of list).</summary>
    Initial,

    /// <summary>The user edited the search box — reset.</summary>
    Typing,

    /// <summary>The search box was cleared — reset.</summary>
    Clear,

    /// <summary>The sort column/direction changed — reset.</summary>
    Sort,

    /// <summary>A result filter changed — reset.</summary>
    Filter,

    /// <summary>The on-disk index changed (USN-driven refresh) — preserves the
    /// viewport.</summary>
    IndexChanged,

    /// <summary>A volume finished indexing and joined the results — preserves
    /// the viewport.</summary>
    VolumeReady,

    /// <summary>The held result went stale and was re-issued — preserves the
    /// viewport.</summary>
    Stale,
}
