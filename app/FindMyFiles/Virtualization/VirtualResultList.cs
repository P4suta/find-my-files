using System.Collections;
using System.Collections.Specialized;
using Microsoft.UI.Xaml.Data;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
using Windows.Foundation.Collections;

namespace FindMyFiles.Virtualization;

/// <summary>
/// A page of already-fetched rows handed to
/// <see cref="VirtualResultList.Reassign"/>, so the viewport is filled the
/// instant a new result is published — never a placeholder flash.
/// </summary>
public readonly record struct PageSeed(long Page, IReadOnlyList<RowData> Rows);

/// <summary>
/// Random-access data virtualization for ListView: non-generic IList +
/// INotifyCollectionChanged + IItemsRangeInfo (the only combination that
/// works — microsoft-ui-xaml#1809, ADR-0015). The indexer hands out stable
/// placeholder instances and never fetches; RangesChanged drives 64-row page
/// fetches on a background task, and arriving data fills those same
/// instances in place.
///
/// The instance lives as long as the page: new query results arrive through
/// <see cref="Reassign"/> (seeded pages + one Reset), never by swapping the
/// ItemsSource (ADR-0015). An epoch counter makes fetches started against a
/// previous result fall on the floor.
///
/// Membership invariant — the WinRT IList adapter trusts these answers
/// blindly: **never vouch for membership falsely.** A false "absent" merely
/// re-realizes a container; a false "present" sends the ListView to
/// GetAt(staleIndex) and dies deep inside XAML (ADR-0015). A row is a member
/// iff its index is inside <see cref="Count"/> AND the current page cache
/// holds that exact instance in that slot; rows from previous results,
/// evicted pages or transient enumeration answer absent. Mutating entry
/// points enforce the UI thread (always, not just in Debug).
/// </summary>
public sealed class VirtualResultList : IList, INotifyCollectionChanged, IItemsRangeInfo
{
    internal const int PageSize = 64;
    private const int MaxCachedPages = 64; // ≈4096 rows

    private readonly IDispatcher _dispatcher;
    private readonly Dictionary<long, ResultRow[]> _pages = [];
    private readonly LinkedList<long> _lru = [];
    private readonly HashSet<long> _loaded = [];
    private readonly HashSet<long> _inFlight = [];
    private ISearchResult? _result;
    private int _epoch;
    private bool _disposed;
    private bool _fetchFailureNotified;

    /// <summary>Raised on the UI thread when a fetch reported staleness.</summary>
    public event Action? BecameStale;

    /// <summary>Raised (Reset) on the UI thread by <see cref="Reassign"/>.</summary>
    public event NotifyCollectionChangedEventHandler? CollectionChanged;

    public VirtualResultList(IDispatcher dispatcher)
    {
        _dispatcher = dispatcher;
    }

    public int Count { get; private set; }

    /// <summary>
    /// Visible (first, last) indexes from the most recent RangesChanged —
    /// what position-preserving requeries prefetch before publishing.
    /// </summary>
    public (int First, int Last)? LastVisibleRange { get; private set; }

    /// <summary>
    /// Atomically replace the backing result: bump the epoch (in-flight
    /// fetches for the old result drop their data), drop the page cache,
    /// apply the pre-fetched seeds, then raise one Reset. UI thread only —
    /// a cross-thread CollectionChanged crashes XAML.
    /// </summary>
    public void Reassign(ISearchResult? result, IReadOnlyList<PageSeed> seeds)
    {
        EnsureUiThread(nameof(Reassign));
        _epoch++;
        var old = _result;
        _result = result;
        _pages.Clear();
        _lru.Clear();
        _loaded.Clear();
        _inFlight.Clear();
        Count = (int)Math.Min(result?.Count ?? 0, int.MaxValue);
        LastVisibleRange = null;
        foreach (var seed in seeds)
        {
            ApplySeed(seed);
        }
        old?.Dispose();
        CollectionChanged?.Invoke(
            this, new NotifyCollectionChangedEventArgs(NotifyCollectionChangedAction.Reset));
    }

    /// <summary>
    /// Swap to a result the engine verified to contain the same rows
    /// (<see cref="QueryTraceData.Unchanged"/>) without raising Reset:
    /// realized rows keep their instances and re-fill from the seeds (the
    /// MVVM setters only notify on actual value changes, so an idle USN
    /// requery repaints nothing). Cached pages are marked unloaded so later
    /// scrolling re-fetches from the new handle, and the visible range is
    /// re-ensured immediately for pages the seeds missed. UI thread only.
    /// </summary>
    public void RefreshInPlace(ISearchResult result, IReadOnlyList<PageSeed> seeds)
    {
        EnsureUiThread(nameof(RefreshInPlace));
        if ((int)Math.Min(result.Count, int.MaxValue) != Count)
        {
            // The engine guarantees identical results on this path; if that
            // ever breaks, a full seeded Reset is the screen-consistent
            // fallback — silently keeping a mismatched count is exactly the
            // membership lie this class must never tell.
            FileLog.Warn(
                "virtualization",
                $"RefreshInPlace count mismatch ({result.Count} vs {Count}) — falling back to Reassign");
            Reassign(result, seeds);
            return;
        }
        _epoch++;
        var old = _result;
        _result = result;
        _loaded.Clear();
        _inFlight.Clear();
        foreach (var seed in seeds)
        {
            ApplySeed(seed);
        }
        old?.Dispose();
        if (LastVisibleRange is { } visible)
        {
            EnsureRange(visible.First, visible.Last);
        }
    }

    /// <summary>Always-on (not Debug-only): a cross-thread mutation reaches
    /// XAML as a marshaling crash far from the cause — fail loud here.</summary>
    private void EnsureUiThread(string member)
    {
        if (!_dispatcher.HasThreadAccess)
        {
            throw new InvalidOperationException($"{member} must run on the UI thread");
        }
    }

    private void ApplySeed(PageSeed seed)
    {
        if (seed.Rows.Count == 0)
        {
            return;
        }
        var rows = GetOrCreatePage(seed.Page);
        for (var i = 0; i < seed.Rows.Count && i < rows.Length; i++)
        {
            rows[i].Fill(seed.Rows[i]);
        }
        _loaded.Add(seed.Page);
    }

    public object? this[int index]
    {
        get
        {
            // Out of range must throw — never fetch, never fabricate
            // phantom pages into the LRU (ADR-0015).
            if ((uint)index >= (uint)Count)
            {
                throw new ArgumentOutOfRangeException(nameof(index), index, $"Count={Count}");
            }
            return GetOrCreatePage(index / PageSize)[index % PageSize];
        }
        set => throw new NotSupportedException();
    }

    private ResultRow[] GetOrCreatePage(long page)
    {
        if (_pages.TryGetValue(page, out var rows))
        {
            Touch(page);
            return rows;
        }
        rows = new ResultRow[PageSize];
        for (var i = 0; i < PageSize; i++)
        {
            rows[i] = ResultRow.CreatePlaceholder(page * PageSize + i);
        }
        _pages[page] = rows;
        _lru.AddFirst(page);
        EvictIfNeeded();
        return rows;
    }

    public void RangesChanged(ItemIndexRange visibleRange, IReadOnlyList<ItemIndexRange> trackedItems) =>
        NotifyVisibleRange(visibleRange.FirstIndex, visibleRange.LastIndex);

    /// <summary>WinRT-free body of RangesChanged (unit-testable). An empty
    /// list reports (-1,-1); remembering that would poison every later
    /// position-preserving requery with Items[-1].</summary>
    internal void NotifyVisibleRange(int firstVisible, int lastVisible)
    {
        if (firstVisible < 0 || lastVisible < firstVisible)
        {
            LastVisibleRange = null;
            return;
        }
        LastVisibleRange = (firstVisible, lastVisible);
        EnsureRange(firstVisible, lastVisible);
    }

    /// <summary>
    /// Kick background fetches for the pages covering the given visible
    /// range ± one page of buffer. WinRT-free seam (unit-testable).
    /// </summary>
    internal void EnsureRange(int firstVisible, int lastVisible)
    {
        if (_disposed || Count == 0 || _result is not { } result)
        {
            return;
        }
        var first = Math.Max(0, firstVisible - PageSize);
        var last = Math.Min(Count - 1, lastVisible + PageSize);
        for (var page = (long)(first / PageSize); page <= last / PageSize; page++)
        {
            if (!_loaded.Contains(page) && _inFlight.Add(page))
            {
                FetchPageAsync(result, page, _epoch).Forget("virtualization.fetch");
            }
        }
    }

    private async Task FetchPageAsync(ISearchResult result, long page, int epoch)
    {
        try
        {
            var data = await result.GetRangeAsync(page * PageSize, PageSize).ConfigureAwait(false);
            _dispatcher.TryEnqueue(() =>
            {
                // Epoch check first: after a Reassign this fetch belongs to a
                // dead result — touching _inFlight/_pages would corrupt the
                // new result's bookkeeping.
                if (epoch != _epoch || _disposed)
                {
                    return;
                }
                _inFlight.Remove(page);
                var rows = GetOrCreatePage(page);
                for (var i = 0; i < data.Count && i < rows.Length; i++)
                {
                    rows[i].Fill(data[i]);
                }
                _loaded.Add(page);
            });
        }
        catch (StaleResultException)
        {
            _dispatcher.TryEnqueue(() =>
            {
                if (epoch != _epoch || _disposed)
                {
                    return; // a requery already replaced this result — no stale storm
                }
                _inFlight.Remove(page);
                BecameStale?.Invoke();
            });
        }
        catch (ObjectDisposedException)
        {
            // Result freed mid-flight — the list moved on to a newer result.
        }
        catch (Exception ex)
        {
            // Anything else is a real bug: log it, tell the user once (not
            // once per page — scrolling would cause a notification storm).
            FileLog.Error("virtualization", $"page fetch failed (page {page})", ex);
            if (!_fetchFailureNotified)
            {
                _fetchFailureNotified = true;
                Notifier.Post(
                    NotifySeverity.Error,
                    "結果の読み込みでエラーが発生しました",
                    ex.Message);
            }
            _dispatcher.TryEnqueue(() =>
            {
                if (epoch == _epoch)
                {
                    _inFlight.Remove(page);
                }
            });
        }
    }

    private void Touch(long page)
    {
        if (_lru.Count > MaxCachedPages / 2 && _lru.First?.Value != page)
        {
            _lru.Remove(page);
            _lru.AddFirst(page);
        }
    }

    private void EvictIfNeeded()
    {
        while (_pages.Count > MaxCachedPages && _lru.Last is { } last)
        {
            _pages.Remove(last.Value);
            _loaded.Remove(last.Value);
            _lru.RemoveLast();
        }
    }

    public void Dispose()
    {
        _disposed = true;
        _result?.Dispose();
    }

    // ── IList boilerplate (read-only) ───────────────────────────────────
    public bool IsFixedSize => true;
    public bool IsReadOnly => true;
    public bool IsSynchronized => false;
    public object SyncRoot => this;
    public int Add(object? value) => throw new NotSupportedException();
    public void Clear() => throw new NotSupportedException();

    // After a Reset the ListView re-locates its selected/focused item
    // through Contains/IndexOf. Answer only for rows whose slot still holds
    // that exact instance — vouching for a row of a previous result sends
    // XAML to GetAt(staleIndex) and crashes (ADR-0015).
    public bool Contains(object? value) => IndexOf(value) >= 0;

    public int IndexOf(object? value)
    {
        if (value is not ResultRow r || r.Index >= Count)
        {
            return -1;
        }
        return _pages.TryGetValue(r.Index / PageSize, out var rows)
            && ReferenceEquals(rows[r.Index % PageSize], r)
            ? (int)r.Index
            : -1;
    }
    public void Insert(int index, object? value) => throw new NotSupportedException();
    public void Remove(object? value) => throw new NotSupportedException();
    public void RemoveAt(int index) => throw new NotSupportedException();

    /// <summary>Read surface stays landmine-free: copy what is cached, hand
    /// out transient placeholders for the rest (never cached — see
    /// <see cref="GetEnumerator"/>).</summary>
    public void CopyTo(Array array, int index)
    {
        ArgumentNullException.ThrowIfNull(array);
        ArgumentOutOfRangeException.ThrowIfNegative(index);
        if (array.Length - index < Count)
        {
            throw new ArgumentException("destination array too small", nameof(array));
        }
        for (var i = 0; i < Count; i++)
        {
            array.SetValue(RowAtWithoutCaching(i), index + i);
        }
    }

    /// <summary>
    /// Enumeration must not disturb the virtualization state: walking a
    /// million-row result through the cache would evict every realized
    /// viewport page (placeholder flash + refetch). Cached slots yield
    /// their live instances; everything else yields transient placeholders,
    /// which by the membership invariant safely answer "absent".
    /// </summary>
    public IEnumerator GetEnumerator()
    {
        for (var i = 0; i < Count; i++)
        {
            yield return RowAtWithoutCaching(i);
        }
    }

    private ResultRow RowAtWithoutCaching(int index) =>
        _pages.TryGetValue(index / PageSize, out var rows)
            ? rows[index % PageSize]
            : ResultRow.CreatePlaceholder(index);
}
