using System.Runtime.InteropServices;

namespace FindMyFiles.ViewModels;

/// <summary>
/// Describes one published result set so the view can place the viewport:
/// reset origins scroll to the top, position-preserving origins scroll to
/// <see cref="RestoreIndex"/>. The seeded index window is where a previously
/// selected row may be re-found.
/// </summary>
/// <param name="Origin">Why the requery ran — decides reset vs. position
/// restore.</param>
/// <param name="RestoreIndex">First visible row index to scroll back to for a
/// position-preserving origin; <c>null</c> for reset origins (scroll to top).</param>
/// <param name="FirstSeededIndex">First row index that was prefetched and is
/// thus realizable without a fetch — lower bound of the selection re-find
/// window.</param>
/// <param name="LastSeededIndex">Last prefetched row index — upper bound of the
/// selection re-find window.</param>
[StructLayout(LayoutKind.Auto)]
public readonly record struct ResultsPublication(
    RequeryOrigin Origin,
    int? RestoreIndex,
    int FirstSeededIndex,
    int LastSeededIndex);
