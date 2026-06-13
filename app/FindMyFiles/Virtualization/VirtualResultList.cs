using System.Collections;
using System.Collections.Specialized;
using Microsoft.UI.Xaml.Data;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;

namespace FindMyFiles.Virtualization;

/// <summary>
/// A page of already-fetched rows handed to
/// <see cref="VirtualResultList.Reassign"/>, so the viewport is filled the
/// instant a new result is published — never a placeholder flash.
/// </summary>
/// <param name="Page">Page index (row index ÷ <see cref="VirtualResultList.PageSize"/>)
/// these rows belong to.</param>
/// <param name="Rows">The page's rows in slot order, as fetched from the engine.</param>
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
[System.Diagnostics.CodeAnalysis.SuppressMessage(
    "Design",
    "CA1010:Generic interface should also be implemented",
    Justification = "WinUI data virtualization requires the non-generic IList surface (microsoft-ui-xaml#1809); generic-only does not virtualize")]
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

    // Per-epoch cancellation, second line of defense behind the epoch check:
    // the epoch check makes late completions fall on the floor; the token
    // stops them from wasting transport work first. Both must stay.
    private CancellationTokenSource _fetchCts = new();
    private bool _disposed;
    private bool _fetchFailureNotified;

    /// <summary>Raised on the UI thread when a fetch reported staleness.</summary>
    public event Action? BecameStale;

    /// <summary>Raised (Reset) on the UI thread by <see cref="Reassign"/>.</summary>
    public event NotifyCollectionChangedEventHandler? CollectionChanged;

    /// <summary>
    /// Bind the list to <paramref name="dispatcher"/>, the UI-thread gate every
    /// mutation is checked against (<see cref="EnsureUiThread"/>) and the queue
    /// background fetch completions marshal back through.
    /// </summary>
    public VirtualResultList(IDispatcher dispatcher)
    {
        _dispatcher = dispatcher;
    }

    /// <summary>Row count of the published result, clamped to
    /// <see cref="int.MaxValue"/> — the fixed size the ListView virtualizes
    /// against. Out-of-range indexers throw rather than fetch.</summary>
    public int Count { get; private set; }

    /// <summary>
    /// Visible (first, last) indexes from the most recent RangesChanged —
    /// what position-preserving requeries prefetch before publishing.
    /// </summary>
    public (int First, int Last)? LastVisibleRange { get; private set; }

    /// <summary>
    /// Atomically replace the backing result: bump the epoch (in-flight
    /// fetches for the old result are cancelled, and their completions drop
    /// their data either way), drop the page cache, apply the pre-fetched
    /// seeds, then raise one Reset.
    ///
    /// Contract: UI thread only (a cross-thread CollectionChanged crashes
    /// XAML); <paramref name="seeds"/> must be pages of
    /// <paramref name="result"/> within its count; ownership of
    /// <paramref name="result"/> transfers to this list (it is disposed by
    /// the next Reassign/RefreshInPlace or by <see cref="Dispose"/>).
    /// </summary>
    public void Reassign(ISearchResult? result, IReadOnlyList<PageSeed> seeds)
    {
        EnsureUiThread(nameof(Reassign));
        _epoch++;
        _fetchCts.Cancel();
        _fetchCts = new CancellationTokenSource();
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
    /// re-ensured immediately for pages the seeds missed.
    ///
    /// Contract: UI thread only; the engine must have verified
    /// <paramref name="result"/> holds the same rows as the published one —
    /// same Count required, a mismatch falls back to a full seeded
    /// <see cref="Reassign"/> (Reset) rather than lying about membership;
    /// seeds must be pages of <paramref name="result"/>; ownership of
    /// <paramref name="result"/> transfers to this list. In-flight fetches
    /// of the previous epoch are cancelled here too.
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
        _fetchCts.Cancel();
        _fetchCts = new CancellationTokenSource();
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

    /// <summary>
    /// Hands out the stable <see cref="ResultRow"/> instance for
    /// <paramref name="index"/> (creating the placeholder page on demand) —
    /// it never fetches, so realization stays cheap; arriving data fills these
    /// same instances in place. The setter is unsupported (read-only list).
    /// </summary>
    /// <param name="index">Zero-based row index; must be inside
    /// <see cref="Count"/> or it throws (an out-of-range slot is never
    /// fabricated into the LRU — ADR-0015).</param>
    /// <exception cref="ArgumentOutOfRangeException"><paramref name="index"/>
    /// is negative or ≥ <see cref="Count"/>.</exception>
    /// <exception cref="NotSupportedException">On set.</exception>
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

    /// <summary>
    /// ListView callback whenever the realized viewport moves; records the
    /// range and kicks page fetches. Delegates to the WinRT-free
    /// <see cref="NotifyVisibleRange"/> seam; <paramref name="trackedItems"/>
    /// (the pinned/selected items) is unused — only the visible window drives
    /// prefetch.
    /// </summary>
    /// <param name="visibleRange">First/last realized item indexes.</param>
    /// <param name="trackedItems">Items the host asked to keep tracked; ignored.</param>
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
                FetchPageAsync(result, page, _epoch, _fetchCts.Token)
                    .Forget("virtualization.fetch");
            }
        }
    }

    private async Task FetchPageAsync(
        ISearchResult result, long page, int epoch, CancellationToken ct)
    {
        try
        {
            var data = await result.GetRangeAsync(page * PageSize, PageSize, ct)
                .ConfigureAwait(false);
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
        catch (OperationCanceledException)
        {
            // Cancelled by an epoch turn (Reassign/RefreshInPlace) or
            // Dispose — the second defense fired; nothing to fill. If this
            // epoch is somehow still live, free the slot so the page can be
            // re-fetched by a later visible-range pass.
            _dispatcher.TryEnqueue(() =>
            {
                if (epoch == _epoch && !_disposed)
                {
                    _inFlight.Remove(page);
                }
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

    /// <summary>
    /// Tear the list down at page end: cancel in-flight fetches, dispose the
    /// fetch token source, and dispose the owned result. Idempotent in effect —
    /// the <c>_disposed</c> guard makes any late fetch continuation bail before
    /// it touches the now-freed source.
    /// </summary>
    public void Dispose()
    {
        _disposed = true;
        // Cancel first so in-flight fetches stop, then dispose: _disposed is
        // already set, so EnsureRange won't read the token again and queued
        // continuations bail on the _disposed guard — nothing observes the
        // source after this point.
        _fetchCts.Cancel();
        _fetchCts.Dispose();
        _result?.Dispose();
    }

    // ── IList boilerplate (read-only) ───────────────────────────────────

    /// <inheritdoc/>
    public bool IsFixedSize => true;

    /// <inheritdoc/>
    public bool IsReadOnly => true;

    /// <inheritdoc/>
    public bool IsSynchronized => false;

    /// <inheritdoc/>
    public object SyncRoot => this;

    /// <inheritdoc/>
    /// <exception cref="NotSupportedException">Always — the list is read-only.</exception>
    public int Add(object? value) => throw new NotSupportedException();

    /// <inheritdoc/>
    /// <exception cref="NotSupportedException">Always — the list is read-only.</exception>
    public void Clear() => throw new NotSupportedException();

    /// <summary>
    /// True only for a row whose slot still holds that exact instance. After a
    /// Reset the ListView re-locates its selected/focused item through
    /// Contains/IndexOf; vouching for a row of a previous result would send XAML
    /// to GetAt(staleIndex) and crash (ADR-0015), so membership is never faked.
    /// </summary>
    public bool Contains(object? value) => IndexOf(value) >= 0;

    /// <summary>
    /// Index of <paramref name="value"/>, or -1, under the same membership
    /// invariant as <see cref="Contains"/>: a <see cref="ResultRow"/> matches
    /// only when its <see cref="ResultRow.Index"/> is inside <see cref="Count"/>
    /// AND the current page cache holds that exact instance in that slot. Rows
    /// from previous results or evicted pages answer absent.
    /// </summary>
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
    /// <inheritdoc/>
    /// <exception cref="NotSupportedException">Always — the list is read-only.</exception>
    public void Insert(int index, object? value) => throw new NotSupportedException();

    /// <inheritdoc/>
    /// <exception cref="NotSupportedException">Always — the list is read-only.</exception>
    public void Remove(object? value) => throw new NotSupportedException();

    /// <inheritdoc/>
    /// <exception cref="NotSupportedException">Always — the list is read-only.</exception>
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
