using FindMyFiles.Engine;
using FindMyFiles.Highlighting;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using FindMyFiles.Virtualization;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary> Defense-in-depth edge cases for the result virtualization pipeline (<see cref="VirtualResultList"/> + <see cref="ResultsPresenter"/>) that the existing per-component suites leave open: that an old epoch's dropped fetch does not poison the <em>new</em> epoch's page bookkeeping, that an in-place refresh updates a different cell (mtime) on the surviving instance, that a storm of failing pages in one result set still notifies exactly once, and that the count-mismatch / superseded transitions through <see cref="ResultsPresenter.RefreshInPlaceAsync"/> keep the previous screen.
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
        SyncContext.RunContinuationsInline();
        var old = new StubSearchResult(Rows.Many(200, "old"))
        {
            Gate = new TaskCompletionSource(),
        };
        _list.Reassign(old, []);
        _list.EnsureRange(0, 0);
        Assert.Equal(2, old.FetchCount);

        var newRows = Rows.Many(200, "new");
        var fresh = new StubSearchResult(newRows);
        _list.Reassign(fresh, SeedPage0(newRows));

        old.Gate!.SetResult();
        _dispatcher.DrainQueue();

        _list.EnsureRange(0, 5);
        Assert.Equal(1, fresh.FetchCount);

        _dispatcher.DrainQueue();
        Assert.Equal("new_000070.txt", Row(70).Name);
    }

    [Fact]
    public void RefreshInPlace_UpdatesMtimeCell_OnTheSurvivingInstance_LeavingPeersUntouched()
    {
        var oldRows = Rows.Many(10);
        _list.Reassign(new StubSearchResult(oldRows), SeedPage0(oldRows));
        var row0Before = Row(0);
        var row1Before = Row(1);
        Assert.Equal(string.Empty, row0Before.DateText);

        var newRows = Rows.Many(10);
        newRows[0] = newRows[0] with { Mtime = new DateTime(2026, 6, 29).ToFileTime() };
        _list.RefreshInPlace(new StubSearchResult(newRows), SeedPage0(newRows));

        Assert.Same(row0Before, Row(0));
        Assert.NotEqual(string.Empty, Row(0).DateText, StringComparer.Ordinal);
        Assert.Same(row1Before, Row(1));
        Assert.Equal(string.Empty, Row(1).DateText);
    }

    [Fact]
    public void PageFetchFailure_ManyFailingPagesInOneResultSet_NotifiesExactlyOnce()
    {
        SyncContext.RunContinuationsInline();
        var expected = Loc.Get("Virtualization_PageFetchFailed");
        var mine = new List<AppNotification>();
        void Handler(AppNotification n)
        {
            if (string.Equals(n.Message, expected, StringComparison.Ordinal))
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

            _list.EnsureRange(0, 320);
            Assert.True(result.FetchCount >= 3, "the test must really kick several pages");

            _dispatcher.DrainQueue();
            Assert.Single(mine);
        }
        finally
        {
            Notifier.Posted -= Handler;
        }
    }

    [Fact]
    public async Task RefreshInPlaceAsync_CountMismatchFallback_SupersededMidFlight_KeepsPreviousScreen()
    {
        await _presenter.PublishAsync(
            new StubSearchResult(Rows.Many(5)),
            null,
            RequeryOrigin.Initial,
            CompiledHighlighter.Empty,
            () => true,
            TestContext.Current.CancellationToken);
        var pubs = new List<ResultsPublication>();
        _presenter.ResultsPublished += pubs.Add;

        var superseded = new StubSearchResult(Rows.Many(8));
        await _presenter.RefreshInPlaceAsync(
            superseded,
            null,
            RequeryOrigin.IndexChanged,
            CompiledHighlighter.Empty,
            () => false,
            TestContext.Current.CancellationToken);

        Assert.True(superseded.Disposed);
        Assert.Empty(pubs);
        Assert.Equal(5, _presenter.ResultsSource.Count);
    }

    [Fact]
    public async Task RefreshInPlaceAsync_SameCountSuperseded_LeavesTheBoundRowsUntouched()
    {
        await _presenter.PublishAsync(
            new StubSearchResult(Rows.Many(5, "old")),
            null,
            RequeryOrigin.Initial,
            CompiledHighlighter.Empty,
            () => true,
            TestContext.Current.CancellationToken);
        var row0 = Assert.IsType<ResultRow>(_presenter.ResultsSource[0]);
        Assert.Equal("old_000000.txt", row0.Name);

        var superseded = new StubSearchResult(Rows.Many(5, "new"));
        await _presenter.RefreshInPlaceAsync(
            superseded,
            null,
            RequeryOrigin.IndexChanged,
            CompiledHighlighter.Empty,
            () => false,
            TestContext.Current.CancellationToken);

        Assert.True(superseded.Disposed);
        Assert.Same(row0, Assert.IsType<ResultRow>(_presenter.ResultsSource[0]));
        Assert.Equal("old_000000.txt", row0.Name);
    }
}
