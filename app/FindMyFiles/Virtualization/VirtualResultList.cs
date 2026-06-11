using System.Collections;
using System.Collections.Specialized;
using System.Diagnostics;
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
/// works — CLAUDE.md UI固定則). The indexer hands out stable placeholder
/// instances and never fetches; RangesChanged drives 64-row page fetches on
/// a background task, and arriving data fills those same instances in place
/// (INotifyPropertyChanged updates the bindings — no container churn).
///
/// The instance lives as long as the page: new query results arrive through
/// <see cref="Reassign"/> (seeded pages + one Reset), never by swapping the
/// ItemsSource — swapping resets the ListView's virtualization state and is
/// what made the screen flicker on every keystroke. An epoch counter makes
/// fetches started against a previous result fall on the floor.
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
        Debug.Assert(_dispatcher.HasThreadAccess, "Reassign must run on the UI thread");
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
        Debug.Assert(_dispatcher.HasThreadAccess, "RefreshInPlace must run on the UI thread");
        Debug.Assert(
            (int)Math.Min(result.Count, int.MaxValue) == Count,
            "RefreshInPlace requires an identical result (caller falls back to Reassign)");
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
        get => GetOrCreatePage(index / PageSize)[index % PageSize];
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

    public void RangesChanged(ItemIndexRange visibleRange, IReadOnlyList<ItemIndexRange> trackedItems)
    {
        LastVisibleRange = (visibleRange.FirstIndex, visibleRange.LastIndex);
        EnsureRange(visibleRange.FirstIndex, visibleRange.LastIndex);
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
    public bool Contains(object? value) => value is ResultRow;
    public int IndexOf(object? value) => value is ResultRow r ? (int)r.Index : -1;
    public void Insert(int index, object? value) => throw new NotSupportedException();
    public void Remove(object? value) => throw new NotSupportedException();
    public void RemoveAt(int index) => throw new NotSupportedException();
    public void CopyTo(Array array, int index) => throw new NotSupportedException();

    public IEnumerator GetEnumerator()
    {
        for (var i = 0; i < Count; i++)
        {
            yield return this[i];
        }
    }
}
