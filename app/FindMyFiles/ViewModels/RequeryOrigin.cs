namespace FindMyFiles.ViewModels;

/// <summary>
/// Why a requery ran. There are exactly two families and every member below
/// belongs to one of them: an origin the user caused resets the viewport to the
/// top of the list, while an origin the engine caused restores the previous top
/// visible row (and re-selects best-effort, only when a seed row's EntryRef
/// still matches) — an index update must never scroll the list out from under
/// the user.
/// </summary>
internal enum RequeryOrigin
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
