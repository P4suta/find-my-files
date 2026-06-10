using System.Collections.Specialized;
using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using FindMyFiles.Virtualization;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class VirtualResultListTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly VirtualResultList _list;

    public VirtualResultListTests()
    {
        _list = new VirtualResultList(_dispatcher);
    }

    private static List<PageSeed> SeedPage0(IReadOnlyList<RowData> rows) =>
        [new PageSeed(0, [.. rows.Take(VirtualResultList.PageSize)])];

    private ResultRow Row(int index) => Assert.IsType<ResultRow>(_list[index]);

    [Fact]
    public void Reassign_AppliesCountAndSeeds_AndRaisesExactlyOneReset()
    {
        var events = new List<NotifyCollectionChangedAction>();
        _list.CollectionChanged += (_, e) => events.Add(e.Action);

        var rows = Rows.Many(10);
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));

        Assert.Equal(10, _list.Count);
        Assert.Equal([NotifyCollectionChangedAction.Reset], events);
        Assert.False(Row(0).IsPlaceholder); // seeded rows are filled…
        Assert.False(Row(9).IsPlaceholder);
        Assert.Equal(rows[0].Name, Row(0).Name); // …with the right data
        Assert.Equal(rows[9].Name, Row(9).Name);
    }

    [Fact]
    public void Indexer_UnseededPage_HandsOutPlaceholders_AndNeverFetches()
    {
        var rows = Rows.Many(100);
        var result = new StubSearchResult(rows);
        _list.Reassign(result, SeedPage0(rows)); // page 1 (index 64+) not seeded

        var row = Row(70);

        Assert.True(row.IsPlaceholder);
        Assert.Equal(70, row.Index);
        Assert.Equal(0, result.FetchCount); // the indexer must not fetch
    }

    [Fact]
    public void EnsureRange_KicksFetches_AndRowsFillAfterTheDispatcherDrains()
    {
        var rows = Rows.Many(100, "fetched");
        var result = new StubSearchResult(rows);
        _list.Reassign(result, []);

        _list.EnsureRange(0, 10); // ± one page of buffer → pages 0 and 1

        Assert.Equal(2, result.FetchCount);
        Assert.True(Row(0).IsPlaceholder); // arrived data still queued

        _dispatcher.DrainQueue();

        Assert.False(Row(0).IsPlaceholder);
        Assert.Equal(rows[0].Name, Row(0).Name);
        Assert.Equal(rows[70].Name, Row(70).Name); // page 1 too

        _list.EnsureRange(0, 10); // loaded pages are not refetched
        Assert.Equal(2, result.FetchCount);
    }

    [Fact]
    public void Reassign_MakesInFlightFetchesOfTheOldResultFallOnTheFloor()
    {
        SyncContext.RunContinuationsInline();
        var oldRows = Rows.Many(10, "old");
        var old = new StubSearchResult(oldRows) { Gate = new TaskCompletionSource() };
        _list.Reassign(old, []);
        _list.EnsureRange(0, 9); // one page, held in flight by the gate
        Assert.Equal(1, old.FetchCount);

        var newRows = Rows.Many(10, "new");
        _list.Reassign(new StubSearchResult(newRows), SeedPage0(newRows));

        old.Gate!.SetResult(); // the old fetch finally completes…
        _dispatcher.DrainQueue();

        // …but the epoch check dropped it: the seeded new data is untouched.
        Assert.Equal("new_000000.txt", Row(0).Name);
        Assert.Equal(10, _list.Count);
    }

    [Fact]
    public void EnsureRange_StaleResult_RaisesBecameStaleOnce()
    {
        var result = new StubSearchResult(Rows.Many(10))
        {
            ThrowOnFetch = new StaleResultException(),
        };
        _list.Reassign(result, []);
        var staleEvents = 0;
        _list.BecameStale += () => staleEvents++;

        _list.EnsureRange(0, 9); // 10 rows → a single page → a single fetch
        _dispatcher.DrainQueue();

        Assert.Equal(1, staleEvents);
        Assert.True(Row(0).IsPlaceholder); // nothing was filled
    }

    [Fact]
    public void Reassign_DisposesThePreviousResult()
    {
        var old = new StubSearchResult(Rows.Many(3));
        _list.Reassign(old, []);

        var fresh = new StubSearchResult(Rows.Many(4));
        _list.Reassign(fresh, []);

        Assert.True(old.Disposed);
        Assert.False(fresh.Disposed);
    }
}
