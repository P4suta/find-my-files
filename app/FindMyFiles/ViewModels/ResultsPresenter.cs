using System.Diagnostics;
using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Highlighting;
using FindMyFiles.Services;
using FindMyFiles.Virtualization;

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

/// <summary>Helpers over <see cref="RequeryOrigin"/>.</summary>
public static class RequeryOriginExtensions
{
    /// <summary>
    /// True for origins that restore the previous viewport instead of scrolling
    /// to the top (<see cref="RequeryOrigin.IndexChanged"/>,
    /// <see cref="RequeryOrigin.VolumeReady"/>, <see cref="RequeryOrigin.Stale"/>) —
    /// the background refreshes the user did not initiate.
    /// </summary>
    public static bool PreservesPosition(this RequeryOrigin origin) =>
        origin is RequeryOrigin.IndexChanged or RequeryOrigin.VolumeReady or RequeryOrigin.Stale;
}

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
public readonly record struct ResultsPublication(
    RequeryOrigin Origin,
    int? RestoreIndex,
    int FirstSeededIndex,
    int LastSeededIndex);

/// <summary>
/// How results are presented: prefetches the pages the viewport will need,
/// then atomically publishes the new result into the lifetime-single
/// <see cref="VirtualResultList"/> (seeded Reset — the old result stays on
/// screen until the new one is ready, so nothing ever flickers). Also owns
/// the result-count status text.
/// </summary>
public sealed partial class ResultsPresenter : ObservableObject
{
    private readonly IDispatcher _dispatcher;

    /// <summary>Lifetime-single ItemsSource — bind with x:Bind OneTime.</summary>
    public VirtualResultList ResultsSource { get; }

    /// <summary>Status-bar text describing the published result — hit count
    /// (and query time when a trace is present), a query-error message, or
    /// empty. Bound one-way to the status bar.</summary>
    [ObservableProperty]
    public partial string CountText { get; set; } = string.Empty;

    /// <summary>Raised on the UI thread right after each seeded Reset.</summary>
    public event Action<ResultsPublication>? ResultsPublished;

    /// <summary>True while the empty-query presentation is on screen, so
    /// repeated index-changed requeries with an empty box are no-ops
    /// (re-Resetting an empty list would flicker the startup screen).
    /// Starts true: the list is born empty.</summary>
    private bool _emptyPresented = true;

    /// <summary>
    /// Create the presenter and its lifetime-single
    /// <see cref="VirtualResultList"/>, both bound to <paramref name="dispatcher"/>
    /// (the UI thread that publishes and fetches run on).
    /// </summary>
    public ResultsPresenter(IDispatcher dispatcher)
    {
        _dispatcher = dispatcher;
        ResultsSource = new VirtualResultList(dispatcher);
    }

    /// <summary>Empty search box → empty screen, idempotently.</summary>
    public void PresentEmpty()
    {
        if (_emptyPresented)
        {
            return;
        }
        _emptyPresented = true;
        ResultsSource.Reassign(null, [], CompiledHighlighter.Empty);
        CountText = string.Empty;
    }

    /// <summary>
    /// Prefetch the viewport pages of <paramref name="result"/>, then publish
    /// it. Runs on the UI thread; the page reads themselves are async, so the
    /// thread is never blocked and newer keystrokes keep flowing. When
    /// <paramref name="isCurrent"/> turns false mid-flight the result is
    /// disposed unpublished — the screen keeps showing the previous result.
    /// </summary>
    /// <exception cref="StaleResultException">
    /// The index was structurally rebuilt while prefetching — the caller
    /// decides whether to retry.
    /// </exception>
    public async Task PublishAsync(
        ISearchResult result,
        QueryTraceData? trace,
        RequeryOrigin origin,
        IHighlighter highlighter,
        Func<bool> isCurrent)
    {
        Debug.Assert(_dispatcher.HasThreadAccess, "PublishAsync must start on the UI thread");

        var count = (int)Math.Min(result.Count, int.MaxValue);
        var (firstIndex, lastIndex, restoreIndex) = SeedWindow(origin, count);

        var seeds = new List<PageSeed>();
        try
        {
            for (var page = firstIndex / VirtualResultList.PageSize;
                 page <= lastIndex / VirtualResultList.PageSize && count > 0;
                 page++)
            {
                var rows = await result.GetRangeAsync(
                    (long)page * VirtualResultList.PageSize, VirtualResultList.PageSize);
                seeds.Add(new PageSeed(page, rows));
            }
        }
        catch
        {
            result.Dispose();
            throw;
        }

        if (!isCurrent())
        {
            result.Dispose(); // superseded while prefetching — keep the old screen
            return;
        }

        _emptyPresented = false;
        ResultsSource.Reassign(result, seeds, highlighter);
        CountText = StatusFormatter.Count(trace, result.Count);
        ResultsPublished?.Invoke(new ResultsPublication(origin, restoreIndex, firstIndex, lastIndex));
    }

    /// <summary>
    /// Same-results refresh (<see cref="QueryTraceData.Unchanged"/>): swap
    /// the new handle in without a Reset, so an idle USN requery repaints
    /// nothing — only cells whose values actually changed in place
    /// (sizes/mtimes of files being written) update. The count text stays
    /// untouched on purpose: a churning ms display reads as flicker.
    /// Falls back to a full publish if the counts somehow disagree.
    /// </summary>
    public async Task RefreshInPlaceAsync(
        ISearchResult result,
        QueryTraceData? trace,
        RequeryOrigin origin,
        IHighlighter highlighter,
        Func<bool> isCurrent)
    {
        Debug.Assert(_dispatcher.HasThreadAccess, "RefreshInPlaceAsync must start on the UI thread");

        var count = (int)Math.Min(result.Count, int.MaxValue);
        if (count != ResultsSource.Count)
        {
            await PublishAsync(result, trace, origin, highlighter, isCurrent);
            return;
        }

        // Always the position-preserving window: the screen is not moving.
        var (firstIndex, lastIndex, _) = SeedWindow(RequeryOrigin.IndexChanged, count);
        var seeds = new List<PageSeed>();
        try
        {
            for (var page = firstIndex / VirtualResultList.PageSize;
                 page <= lastIndex / VirtualResultList.PageSize && count > 0;
                 page++)
            {
                var rows = await result.GetRangeAsync(
                    (long)page * VirtualResultList.PageSize, VirtualResultList.PageSize);
                seeds.Add(new PageSeed(page, rows));
            }
        }
        catch
        {
            result.Dispose();
            throw;
        }

        if (!isCurrent())
        {
            result.Dispose(); // superseded while prefetching — keep the old screen
            return;
        }

        ResultsSource.RefreshInPlace(result, seeds, highlighter);
    }

    /// <summary>Show a query problem without touching the published results.</summary>
    public void PresentQueryError(string message) => CountText = StatusFormatter.QueryError(message);

    /// <summary>Engine failure: the notification carries the details.</summary>
    public void PresentEngineFailure() => CountText = string.Empty;

    /// <summary>
    /// Index window to prefetch: the top two pages for reset origins, the
    /// last-seen viewport (clamped to the new count) ± one page for
    /// position-preserving origins.
    /// </summary>
    private (int First, int Last, int? Restore) SeedWindow(RequeryOrigin origin, int count)
    {
        var lastRow = Math.Max(0, count - 1);
        if (origin.PreservesPosition() && ResultsSource.LastVisibleRange is { } visible)
        {
            // Clamp below as well: an empty viewport reports (-1,-1), and
            // Items[-1] crashes in the WinRT IList adapter (ADR-0015).
            var restore = Math.Clamp(visible.First, 0, lastRow);
            var first = Math.Max(0, restore - VirtualResultList.PageSize);
            var last = Math.Min(lastRow, Math.Min(visible.Last, lastRow) + VirtualResultList.PageSize);
            return (first, last, restore);
        }
        return (0, Math.Min(lastRow, 2 * VirtualResultList.PageSize - 1), null);
    }
}
