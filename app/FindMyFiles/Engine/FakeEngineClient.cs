using System.Text.Json;
using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// Deterministic in-memory engine for UI tests and unelevated development
/// (`--fake-engine`). 100k entries from a fixed seed; substring search only.
/// Contract-conforming (EngineClientContractTests): query-syntax verdicts
/// come from the shared golden fixture (contract/golden/invalid_queries.json,
/// pinned by the real Rust parser — verdict drift is caught on the Rust
/// side), cancellation is honored, and <see cref="BumpEpoch"/> lets tests
/// drive the Stale→requery recovery path without a real engine.
/// </summary>
public sealed class FakeEngineClient : IEngineClient
{
    private const int EntryCount = 100_000;
    private readonly List<RowData> _rows;
    private readonly IReadOnlySet<string> _invalidQueries = LoadInvalidQueries();
    private int _epoch;

    public event Action<string>? IndexChanged { add { } remove { } }
    public event Action<VolumeStatus>? VolumeUpdated { add { } remove { } }
    public event Action<EngineConnectionState>? ConnectionChanged { add { } remove { } }

    /// <summary>In-proc: no transport, no state transitions.</summary>
    public EngineConnectionState Connection => EngineConnectionState.InProc;
    // Only the DEBUG-only fault injection raises this; Release builds keep
    // the member to satisfy IEngineClient.
#pragma warning disable CS0067
    public event Action<int>? EngineErrorOccurred;
#pragma warning restore CS0067

    private readonly List<ErrorEventData> _injectedErrors = [];

    /// <summary>Visibly empty stand-in for the unelevated no-service start —
    /// searching demo rows has no practical value (user verdict), so the
    /// auto-fallback shows zero results plus the setup notification instead.
    /// The data-bearing fake stays what <c>--fake-engine</c> is for.</summary>
    public static FakeEngineClient CreateEmpty() => new(empty: true);

    /// <summary>True for the unelevated auto-fallback instance (no rows) —
    /// the status badge says 未接続, not fake.</summary>
    public bool IsEmpty { get; }

    public FakeEngineClient(bool empty = false)
    {
        IsEmpty = empty;
        if (empty)
        {
            _rows = [];
            return;
        }
        var rng = new Random(42);
        string[] exts = ["txt", "rs", "cs", "dll", "png", "pdf", "log", "json"];
        string[] dirs = ["F:\\", "F:\\src\\", "F:\\docs\\", "F:\\bin\\debug\\", "F:\\photos\\2026\\"];
        _rows = new List<RowData>(EntryCount);
        var baseTime = new DateTimeOffset(2026, 1, 1, 0, 0, 0, TimeSpan.Zero).ToFileTime();
        for (var i = 0; i < EntryCount; i++)
        {
            var isDir = i % 50 == 0;
            // Every 97th entry plays a hidden/system file so the UI toggle is
            // exercisable against deterministic data.
            var isHiddenSystem = i % 97 == 0;
            var name = isHiddenSystem
                ? $"hidden_sys_{i:D6}.dat"
                : isDir
                    ? $"folder_{i:D6}"
                    : $"file_{i:D6}_{(char)('a' + rng.Next(26))}.{exts[rng.Next(exts.Length)]}";
            _rows.Add(new RowData(
                EntryRef: (ulong)i,
                Frn: (uint)i | (1UL << 48),
                Size: isDir ? 0UL : (ulong)rng.Next(0, 1 << 24),
                Mtime: baseTime + (long)i * 10_000_000,
                Flags: (isDir ? 1u : 0u) | (isHiddenSystem ? 4u : 0u),
                Name: name,
                ParentPath: dirs[rng.Next(dirs.Length)]));
        }
    }

    /// <summary>The shared syntax fixture: queries the real engine rejects.
    /// A missing/corrupt file degrades to accept-everything — loudly, never
    /// fatally (the fake must keep working in stripped-down test hosts).</summary>
    private static IReadOnlySet<string> LoadInvalidQueries()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "golden", "invalid_queries.json");
        try
        {
            using var doc = JsonDocument.Parse(File.ReadAllBytes(path));
            return doc.RootElement.GetProperty("queries").EnumerateArray()
                .Select(q => q.GetString()!)
                .ToHashSet(StringComparer.Ordinal);
        }
        catch (Exception ex)
        {
            FileLog.Warn(
                "fake-engine",
                $"invalid_queries.json not loadable ({path}) — accepting every query", ex);
            return new HashSet<string>();
        }
    }

    /// <summary>Test hook: structurally invalidate every result handed out
    /// so far — their next GetRangeAsync throws <see cref="StaleResultException"/>,
    /// exactly like a real index rebuild (UI Stale→requery recovery).</summary>
    public void BumpEpoch() => Interlocked.Increment(ref _epoch);

    public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default) =>
        Task.FromResult<IReadOnlyList<string>>(["F:"]);

    public Task StartIndexingAsync(
        IReadOnlyList<string> volumes, CancellationToken ct = default) => Task.CompletedTask;

    public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default) =>
        Task.FromResult<IReadOnlyList<VolumeStatus>>(
            [new("F:", VolumeState.Ready, (ulong)_rows.Count)]);

    private readonly List<QueryTraceData> _traces = [];

    public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        var stats = new EngineStatsData
        {
            RecentQueries = _traces.TakeLast(64).ToList(),
            P50Us = 1500,
            P99Us = 4000,
            Indexes =
            [
                new IndexStatsData
                {
                    Volume = "F:",
                    Entries = (ulong)_rows.Count,
                    LiveEntries = (ulong)_rows.Count,
                    TotalBytes = (ulong)_rows.Count * 110,
                    BytesPerEntry = 110,
                },
            ],
            RecentErrors = [.. _injectedErrors],
        };
        return Task.FromResult<EngineStatsData?>(stats);
    }

    public Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        if (_invalidQueries.Contains(query.Trim()))
        {
            throw new QuerySyntaxException(
                $"syntax error (shared golden fixture): {query.Trim()}");
        }
#if DEBUG
        // Fault injection for end-to-end verification of the error pipeline
        // (InfoBar, F12 panel, app.log) without touching real volumes.
        if (query.Trim() == "!!panic")
        {
            throw new EngineException("fault injection: simulated engine panic", 99);
        }
        if (query.Trim() == "!!warn")
        {
            _injectedErrors.Add(new ErrorEventData
            {
                Seq = (ulong)_injectedErrors.Count + 1,
                Severity = "warn",
                Area = "fake",
                Volume = "F:",
                Message = "fault injection: simulated warning",
            });
            EngineErrorOccurred?.Invoke(1);
        }
#endif
        var sw = System.Diagnostics.Stopwatch.StartNew();
        var needle = query.Trim();
        var pageLag = TimeSpan.Zero;
#if DEBUG
        // Fault injection: `!!lag` makes every page fetch take 250ms, so the
        // results-publication path can be verified to never show blank rows
        // (the rest of the query still filters normally).
        if (needle.Contains("!!lag", StringComparison.Ordinal))
        {
            pageLag = TimeSpan.FromMilliseconds(250);
            needle = needle.Replace("!!lag", "", StringComparison.Ordinal).Trim();
        }
#endif
        IEnumerable<RowData> hits = needle.Length == 0
            ? _rows
            : _rows.Where(r => r.Name.Contains(needle, StringComparison.OrdinalIgnoreCase));
        if (!options.IncludeHiddenSystem)
        {
            hits = hits.Where(r => (r.Flags & 4) == 0);
        }

        var sorted = options.Sort switch
        {
            FmfSort.Size => hits.OrderBy(r => r.Size),
            FmfSort.Mtime => hits.OrderBy(r => r.Mtime),
            _ => hits.OrderBy(r => r.Name, StringComparer.OrdinalIgnoreCase),
        };
        var list = (options.Descending ? sorted.Reverse() : sorted).ToList();
        var totalUs = (ulong)(sw.Elapsed.TotalMicroseconds + 1);
        var trace = new QueryTraceData
        {
            Query = query,
            Driver = "fake",
            ScanUs = totalUs * 7 / 10,
            MaterializeUs = totalUs * 2 / 10,
            ParseUs = totalUs / 10,
            TotalUs = totalUs,
            EntriesScanned = (ulong)_rows.Count,
            Hits = (ulong)list.Count,
            Volumes = 1,
        };
        _traces.Add(trace);
        if (_traces.Count > 256)
        {
            _traces.RemoveAt(0);
        }
        return Task.FromResult(new SearchOutcome(
            new FakeResult(this, Volatile.Read(ref _epoch), list, pageLag), trace));
    }

    public void Dispose() { }

    private sealed class FakeResult(
        FakeEngineClient owner, int epoch, List<RowData> rows, TimeSpan pageLag) : ISearchResult
    {
        public long Count => rows.Count;

        public async Task<IReadOnlyList<RowData>> GetRangeAsync(
            long offset, int count, CancellationToken ct = default)
        {
            ct.ThrowIfCancellationRequested();
            if (epoch != Volatile.Read(ref owner._epoch))
            {
                throw new StaleResultException(); // BumpEpoch invalidated us
            }
            if (pageLag > TimeSpan.Zero)
            {
                await Task.Delay(pageLag, ct).ConfigureAwait(false);
            }
            var start = (int)Math.Min(offset, rows.Count);
            var n = Math.Max(0, Math.Min(count, rows.Count - start));
            return rows.GetRange(start, n);
        }

        public void Dispose() { }
    }
}
