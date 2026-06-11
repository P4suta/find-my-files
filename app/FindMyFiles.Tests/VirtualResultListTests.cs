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
    public void RefreshInPlace_NoReset_SameRowInstances_AndChangedCellsUpdate()
    {
        var oldRows = Rows.Many(10);
        var old = new StubSearchResult(oldRows);
        _list.Reassign(old, SeedPage0(oldRows));
        var rowBefore = Row(0);
        var events = new List<NotifyCollectionChangedAction>();
        _list.CollectionChanged += (_, e) => events.Add(e.Action);

        // Same ids; one file grew in place (USN stat update).
        var newRows = Rows.Many(10);
        newRows[0] = newRows[0] with { Size = 4096 };
        var fresh = new StubSearchResult(newRows);
        _list.RefreshInPlace(fresh, SeedPage0(newRows));

        Assert.Empty(events); // no Reset — an unchanged screen repaints nothing
        Assert.Same(rowBefore, Row(0)); // bound instances survive the swap
        Assert.Equal("4 KB", Row(0).SizeText); // …but live values still update
        Assert.True(old.Disposed);
        Assert.False(fresh.Disposed);
        Assert.Equal(10, _list.Count);
    }

    [Fact]
    public void RefreshInPlace_DropsLoadedFlags_SoScrollingRefetchesFromTheNewHandle()
    {
        var rows = Rows.Many(100);
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));
        var fresh = new StubSearchResult(rows);
        _list.RefreshInPlace(fresh, SeedPage0(rows));
        Assert.Equal(0, fresh.FetchCount); // the seed covered page 0

        _list.EnsureRange(0, 10); // page 0 re-seeded; page 1 must hit the new handle
        Assert.Equal(1, fresh.FetchCount);
        _dispatcher.DrainQueue();
        Assert.Equal(rows[70].Name, Row(70).Name);
    }

    [Fact]
    public void IndexOf_AnswersOnlyForRowsOfTheCurrentResult()
    {
        var rows = Rows.Many(100);
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));
        var row = Row(5);
        Assert.Equal(5, _list.IndexOf(row));
        Assert.True(_list.Contains(row));

        // After shrinking to empty, rows of the previous result are no
        // longer "in" the collection — a false "present" sends the
        // ListView's Reset handling to GetAt(staleIndex) (ADR-0015).
        _list.Reassign(new StubSearchResult(Rows.Many(0)), []);
        Assert.Equal(-1, _list.IndexOf(row));
        Assert.False(_list.Contains(row));
        Assert.Equal(-1, _list.IndexOf("not a row"));
    }

    [Fact]
    public void NotifyVisibleRange_EmptyViewportReport_IsForgottenNotRemembered()
    {
        var rows = Rows.Many(100);
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));
        _list.NotifyVisibleRange(0, 20);
        Assert.Equal((0, 20), _list.LastVisibleRange);

        // An emptied list reports (-1,-1); remembering it would poison
        // later position-preserving requeries with Items[-1] (ADR-0015).
        _list.NotifyVisibleRange(-1, -1);
        Assert.Null(_list.LastVisibleRange);
    }

    [Fact]
    public void Indexer_OutOfRange_ThrowsInsteadOfFabricatingRows()
    {
        var rows = Rows.Many(10);
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));

        Assert.Throws<ArgumentOutOfRangeException>(() => _list[-1]);
        Assert.Throws<ArgumentOutOfRangeException>(() => _list[10]);
        Assert.NotNull(_list[9]); // bounds are exact, not off by one
    }

    [Fact]
    public void Enumeration_DoesNotDisturbTheVirtualizationState()
    {
        var rows = Rows.Many(10_000); // 157 pages — way past MaxCachedPages
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));
        var viewportRow = Row(0); // realized, seeded instance

        var enumerated = _list.Cast<ResultRow>().Count();

        Assert.Equal(10_000, enumerated);
        // The seeded viewport page survived: same instance, still filled —
        // enumerating must not evict realized pages (placeholder flash).
        Assert.Same(viewportRow, Row(0));
        Assert.False(Row(0).IsPlaceholder);
        // Transient enumeration rows are honestly absent.
        Assert.Equal(-1, _list.IndexOf(_list.Cast<ResultRow>().Last()));
    }

    [Fact]
    public void CopyTo_FillsTheArray_WithoutLandmines()
    {
        var rows = Rows.Many(5);
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));

        var target = new object[7];
        ((System.Collections.ICollection)_list).CopyTo(target, 2);

        Assert.Same(Row(0), target[2]);
        Assert.Equal(rows[4].Name, Assert.IsType<ResultRow>(target[6]).Name);
    }

    [Fact]
    public void RefreshInPlace_CountMismatch_FallsBackToAFullReset()
    {
        var rows = Rows.Many(10);
        _list.Reassign(new StubSearchResult(rows), SeedPage0(rows));
        var events = new List<NotifyCollectionChangedAction>();
        _list.CollectionChanged += (_, e) => events.Add(e.Action);

        // The engine guarantees identical results on this path; if the
        // guarantee ever breaks, the list must re-present, not lie.
        var different = Rows.Many(7);
        _list.RefreshInPlace(new StubSearchResult(different), SeedPage0(different));

        Assert.Equal([NotifyCollectionChangedAction.Reset], events);
        Assert.Equal(7, _list.Count);
        Assert.Equal(different[0].Name, Row(0).Name);
    }

    [Fact]
    public void Mutations_OffTheUiThread_FailLoudAtTheSource()
    {
        var offThread = new OffThreadDispatcher();
        var list = new VirtualResultList(offThread);

        Assert.Throws<InvalidOperationException>(
            () => list.Reassign(new StubSearchResult(Rows.Many(1)), []));
        Assert.Throws<InvalidOperationException>(
            () => list.RefreshInPlace(new StubSearchResult(Rows.Many(1)), []));
    }

    private sealed class OffThreadDispatcher : FindMyFiles.Services.IDispatcher
    {
        public bool HasThreadAccess => false;
        public bool TryEnqueue(Action action) => true;
        public FindMyFiles.Services.IDispatcherTimer CreateOneShotTimer(
            TimeSpan interval, Action tick) => throw new NotSupportedException();
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
