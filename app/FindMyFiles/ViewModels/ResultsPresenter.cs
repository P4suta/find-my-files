using System.Diagnostics;
using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Highlighting;
using FindMyFiles.Services;
using FindMyFiles.Virtualization;

namespace FindMyFiles.ViewModels;

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
    /// <param name="dispatcher">UI-thread dispatcher that publishes and page
    /// fetches run on.</param>
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
    /// <param name="result">The new search result to prefetch and publish.</param>
    /// <param name="trace">Optional query trace used to format the count text;
    /// <c>null</c> when no timing is available.</param>
    /// <param name="origin">Why the requery ran — decides reset vs. position
    /// restore.</param>
    /// <param name="highlighter">Match highlighter applied to the published rows.</param>
    /// <param name="isCurrent">Predicate that returns false once a newer query
    /// has superseded this one.</param>
    /// <exception cref="StaleResultException">
    /// The index was structurally rebuilt while prefetching — the caller
    /// decides whether to retry.
    /// </exception>
    /// <returns>A <see cref="Task"/> representing the asynchronous publish.</returns>
    public async Task PublishAsync(
        ISearchResult result,
        QueryTraceData? trace,
        RequeryOrigin origin,
        IHighlighter highlighter,
        Func<bool> isCurrent)
    {
        Debug.Assert(_dispatcher.HasThreadAccess, "PublishAsync must start on the UI thread");

        // The page reads below must NOT ConfigureAwait(false): the heavy fetch runs
        // off-thread inside GetRangeAsync (FFI Task.Run / pipe I/O), but the
        // continuation resumes here to call ResultsSource.Reassign / set CountText,
        // both of which require the UI thread (EnsureUiThread throws otherwise).
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
    /// <param name="result">The refreshed search result handle to swap in.</param>
    /// <param name="trace">Optional query trace forwarded to the publish
    /// fallback; <c>null</c> when no timing is available.</param>
    /// <param name="origin">Why the requery ran — forwarded to the publish
    /// fallback.</param>
    /// <param name="highlighter">Match highlighter applied to the refreshed rows.</param>
    /// <param name="isCurrent">Predicate that returns false once a newer query
    /// has superseded this one.</param>
    /// <returns>A <see cref="Task"/> representing the asynchronous refresh.</returns>
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
    /// <param name="message">The query-error message to display in the status bar.</param>
    public void PresentQueryError(string message) => CountText = StatusFormatter.QueryError(message);

    /// <summary>Engine failure: the notification carries the details.</summary>
    public void PresentEngineFailure() => CountText = string.Empty;

    /// <summary>
    /// Index window to prefetch: the top two pages for reset origins, the
    /// last-seen viewport (clamped to the new count) ± one page for
    /// position-preserving origins.
    /// </summary>
    /// <param name="origin">Why the requery ran — selects reset vs.
    /// position-preserving window.</param>
    /// <param name="count">Total row count of the new result.</param>
    /// <returns>The first and last row indices to prefetch, and the row index
    /// to restore the viewport to (<c>null</c> for reset origins).</returns>
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

        return (0, Math.Min(lastRow, (2 * VirtualResultList.PageSize) - 1), null);
    }
}
