using FindMyFiles.Engine;
using FindMyFiles.Highlighting;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using FindMyFiles.Virtualization;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Defense-in-depth edge cases for the result virtualization pipeline
/// (<see cref="VirtualResultList"/> + <see cref="ResultsPresenter"/>) that the
/// existing per-component suites leave open: that an old epoch's dropped fetch
/// does not poison the <em>new</em> epoch's page bookkeeping, that an in-place
/// refresh updates a different cell (mtime) on the surviving instance, that a
/// storm of failing pages in one result set still notifies exactly once, and
/// that the count-mismatch / superseded transitions through
/// <see cref="ResultsPresenter.RefreshInPlaceAsync"/> keep the previous screen.
/// Driven by <see cref="StubSearchResult"/> + <see cref="ManualDispatcher"/>.
/// </summary>
public sealed class ResultsVirtualizationEdgeTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly VirtualResultList _list;
    private readonly ResultsPresenter _presenter;

    public ResultsVirtualizationEdgeTests()
    {
        _list = new VirtualResultList(_dispatcher);
        _presenter = new ResultsPresenter(_dispatcher);
    }

    private static List<PageSeed> SeedPage0(IReadOnlyList<RowData> rows) =>
        [new PageSeed(0, [.. rows.Take(VirtualResultList.PageSize)])];

    private ResultRow Row(int index) => Assert.IsType<ResultRow>(_list[index]);

    [Fact]
    public void Reassign_DroppedOldFetch_DoesNotMarkNewEpochPagesLoaded()
    {
        // The companion suite proves the seeded rows survive an old epoch's late
        // completion. This pins the bookkeeping side: a dropped old-epoch fetch
        // must NOT add to the new epoch's _loaded set, so scrolling re-fetches
        // that page from the NEW handle and gets new data — never the stale page
        // the old fetch was carrying.
        SyncContext.RunContinuationsInline();
        var old = new StubSearchResult(Rows.Many(200, "old"))
        {
            Gate = new TaskCompletionSource(),
        };
        _list.Reassign(old, []);
        _list.EnsureRange(0, 0); // pages 0 and 1 in flight on the old epoch
        Assert.Equal(2, old.FetchCount);

        var newRows = Rows.Many(200, "new");
        var fresh = new StubSearchResult(newRows);
        _list.Reassign(fresh, SeedPage0(newRows)); // new epoch; only page 0 seeded

        old.Gate!.SetResult(); // the old page-0/1 fetches resume with old data…
        _dispatcher.DrainQueue(); // …and the epoch check drops both

        // Page 1 was never marked loaded by the dropped completion, so a scroll
        // into it hits the new handle exactly once (page 0 stays covered by the
        // seed and is not re-fetched).
        _list.EnsureRange(0, 5);
        Assert.Equal(1, fresh.FetchCount);

        _dispatcher.DrainQueue();
        Assert.Equal("new_000070.txt", Row(70).Name); // new data, not the dropped old fetch
    }

    [Fact]
    public void RefreshInPlace_UpdatesMtimeCell_OnTheSurvivingInstance_LeavingPeersUntouched()
    {
        // The companion suite pins a size change; this pins a different cell
        // (mtime → DateText) and the contrast against an unchanged peer row: an
        // idle USN requery of a file being written must update only that row's
        // date, in place, while every other bound instance and value is left
        // exactly as it was.
        var oldRows = Rows.Many(10);
        _list.Reassign(new StubSearchResult(oldRows), SeedPage0(oldRows));
        var row0Before = Row(0);
        var row1Before = Row(1);
        Assert.Equal(string.Empty, row0Before.DateText); // mtime 0 → no date yet

        var newRows = Rows.Many(10);
        newRows[0] = newRows[0] with { Mtime = new DateTime(2026, 6, 29).ToFileTime() };
        _list.RefreshInPlace(new StubSearchResult(newRows), SeedPage0(newRows));

        Assert.Same(row0Before, Row(0)); // the written file's row keeps its instance…
        Assert.NotEqual(string.Empty, Row(0).DateText); // …but its date now shows
        Assert.Same(row1Before, Row(1)); // the untouched peer is identical
        Assert.Equal(string.Empty, Row(1).DateText);
    }

    [Fact]
    public void PageFetchFailure_ManyFailingPagesInOneResultSet_NotifiesExactlyOnce()
    {
        // The companion suite proves the notice re-arms ACROSS result sets. This
        // proves the "not once per page" half within a SINGLE set: a viewport
        // that kicks many pages, every one of them faulting, still tells the
        // user exactly once (no notification storm while scrolling).
        SyncContext.RunContinuationsInline();
        var mine = new List<AppNotification>();
        void Handler(AppNotification n)
        {
            if (string.Equals(n.Message, "結果の読み込みでエラーが発生しました", StringComparison.Ordinal))
            {
                mine.Add(n);
            }
        }

        Notifier.Posted += Handler;
        try
        {
            var result = new StubSearchResult(Rows.Many(500))
            {
                ThrowOnFetch = new InvalidOperationException("boom"),
            };
            _list.Reassign(result, []);

            _list.EnsureRange(0, 320); // spans seven pages → seven failing fetches
            Assert.True(result.FetchCount >= 3, "the test must really kick several pages");

            _dispatcher.DrainQueue();
            Assert.Single(mine); // a storm of failing pages is one notice, not seven
        }
        finally
        {
            Notifier.Posted -= Handler;
        }
    }

    [Fact]
    public async Task RefreshInPlaceAsync_CountMismatchFallback_SupersededMidFlight_KeepsPreviousScreen()
    {
        // RefreshInPlaceAsync routes a count mismatch through PublishAsync; the
        // companion presenter suite pins only the happy fallback. This pins that
        // the fallback still honors supersession: a newer query landing before it
        // can publish disposes the result unpublished and leaves the prior screen.
        await _presenter.PublishAsync(
            new StubSearchResult(Rows.Many(5)),
            null,
            RequeryOrigin.Initial,
            CompiledHighlighter.Empty,
            () => true);
        var pubs = new List<ResultsPublication>();
        _presenter.ResultsPublished += pubs.Add;

        var superseded = new StubSearchResult(Rows.Many(8)); // 8 ≠ 5 → fallback path
        await _presenter.RefreshInPlaceAsync(
            superseded,
            null,
            RequeryOrigin.IndexChanged,
            CompiledHighlighter.Empty,
            () => false); // a newer query already won

        Assert.True(superseded.Disposed);
        Assert.Empty(pubs); // the fallback never announced
        Assert.Equal(5, _presenter.ResultsSource.Count); // the prior result stays on screen
    }

    [Fact]
    public async Task RefreshInPlaceAsync_SameCountSuperseded_LeavesTheBoundRowsUntouched()
    {
        // The same-count (true in-place) branch must honor supersession too: a
        // newer query landing mid-prefetch disposes the refresh unpublished and
        // never swaps the handle, so the bound row instances and their values are
        // exactly as the previous publish left them.
        await _presenter.PublishAsync(
            new StubSearchResult(Rows.Many(5, "old")),
            null,
            RequeryOrigin.Initial,
            CompiledHighlighter.Empty,
            () => true);
        var row0 = Assert.IsType<ResultRow>(_presenter.ResultsSource[0]);
        Assert.Equal("old_000000.txt", row0.Name);

        var superseded = new StubSearchResult(Rows.Many(5, "new")); // same count, in-place branch
        await _presenter.RefreshInPlaceAsync(
            superseded,
            null,
            RequeryOrigin.IndexChanged,
            CompiledHighlighter.Empty,
            () => false);

        Assert.True(superseded.Disposed);
        Assert.Same(row0, Assert.IsType<ResultRow>(_presenter.ResultsSource[0])); // instance untouched
        Assert.Equal("old_000000.txt", row0.Name); // the in-place swap never happened
    }
}
