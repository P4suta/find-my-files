using System.Collections;
using System.Collections.Specialized;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Data;
using FindMyFiles.Engine;
using FindMyFiles.ViewModels;
using Windows.Foundation.Collections;

namespace FindMyFiles.Virtualization;

/// <summary>
/// Random-access data virtualization for ListView: non-generic IList +
/// INotifyCollectionChanged + IItemsRangeInfo (the only combination that
/// works — CLAUDE.md UI固定則). The indexer hands out stable placeholder
/// instances and never fetches; RangesChanged drives 64-row page fetches on
/// a background task, and arriving data fills those same instances in place
/// (INotifyPropertyChanged updates the bindings — no container churn).
/// </summary>
public sealed class VirtualResultList : IList, INotifyCollectionChanged, IItemsRangeInfo
{
    private const int PageSize = 64;
    private const int MaxCachedPages = 64; // ≈4096 rows

    private readonly ISearchResult _result;
    private readonly DispatcherQueue _dispatcher;
    private readonly Dictionary<long, ResultRow[]> _pages = [];
    private readonly LinkedList<long> _lru = [];
    private readonly HashSet<long> _loaded = [];
    private readonly HashSet<long> _inFlight = [];
    private bool _disposed;

    /// <summary>Raised on the UI thread when a fetch reported staleness.</summary>
    public event Action? BecameStale;

    public VirtualResultList(ISearchResult result, DispatcherQueue dispatcher)
    {
        _result = result;
        _dispatcher = dispatcher;
        Count = (int)Math.Min(_result.Count, int.MaxValue);
    }

    public int Count { get; }

    // Contract-required interface; we never mutate, so no events fire.
    public event NotifyCollectionChangedEventHandler? CollectionChanged
    {
        add { }
        remove { }
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
        if (_disposed || Count == 0)
        {
            return;
        }
        // Visible range ± one page of buffer in each direction.
        var first = Math.Max(0, visibleRange.FirstIndex - PageSize);
        var last = Math.Min(Count - 1, visibleRange.LastIndex + PageSize);
        for (var page = (long)(first / PageSize); page <= last / PageSize; page++)
        {
            if (!_loaded.Contains(page) && _inFlight.Add(page))
            {
                _ = FetchPageAsync(page);
            }
        }
    }

    private async Task FetchPageAsync(long page)
    {
        try
        {
            var data = await _result.GetRangeAsync(page * PageSize, PageSize).ConfigureAwait(false);
            _dispatcher.TryEnqueue(() =>
            {
                _inFlight.Remove(page);
                if (_disposed)
                {
                    return;
                }
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
                _inFlight.Remove(page);
                BecameStale?.Invoke();
            });
        }
        catch (ObjectDisposedException)
        {
            // Result freed mid-flight — the list is being torn down.
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
        _result.Dispose();
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
