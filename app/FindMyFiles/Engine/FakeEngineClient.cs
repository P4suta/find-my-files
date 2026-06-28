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
    private readonly HashSet<string> _invalidQueries = LoadInvalidQueries();
    private int _epoch;

    /// <inheritdoc/>
    /// <remarks>The fake never indexes, so this never fires (empty
    /// accessors).</remarks>
    public event Action<string>? IndexChanged
    {
        add { } remove { }
    }

    /// <inheritdoc/>
    /// <remarks>Volume state is fixed (one Ready volume), so this never fires
    /// (empty accessors).</remarks>
    public event Action<VolumeStatus>? VolumeUpdated
    {
        add { } remove { }
    }

    /// <inheritdoc/>
    /// <remarks>In-proc has no transport, so this never fires (empty
    /// accessors).</remarks>
    public event Action<EngineConnectionState>? ConnectionChanged
    {
        add { } remove { }
    }

    /// <summary>In-proc: no transport, no state transitions.</summary>
    public EngineConnectionState Connection => EngineConnectionState.InProc;

    // Only the DEBUG-only fault injection raises this; Release builds keep
    // the member to satisfy IEngineClient.
#pragma warning disable CS0067
    /// <inheritdoc/>
    /// <remarks>Only the DEBUG <c>!!warn</c> fault injection raises this; in
    /// Release builds it never fires.</remarks>
    public event Action<int>? EngineErrorOccurred;
#pragma warning restore CS0067

    private readonly List<ErrorEventData> _injectedErrors = [];

    /// <summary>Visibly empty stand-in for the unelevated no-service start —
    /// searching demo rows has no practical value (user verdict), so the
    /// auto-fallback shows zero results plus the setup notification instead.
    /// The data-bearing fake stays what <c>--fake-engine</c> is for.</summary>
    /// <returns>A rowless fake that reports disconnected and yields no results.</returns>
    public static FakeEngineClient CreateEmpty() => new(empty: true);

    /// <summary>True for the unelevated auto-fallback instance (no rows) —
    /// the status badge says disconnected, not fake.</summary>
    public bool IsEmpty { get; }

    /// <summary>Builds the fake. The default (<paramref name="empty"/> false)
    /// generates the deterministic 100k-row dataset that backs
    /// <c>--fake-engine</c>; <see cref="CreateEmpty"/> passes
    /// <paramref name="empty"/> true for the unelevated no-service
    /// stand-in.</summary>
    /// <param name="empty">When true, no rows are generated (the disconnected
    /// auto-fallback); when false, the seeded demo dataset is built.</param>
    public FakeEngineClient(bool empty = false)
    {
        IsEmpty = empty;
        if (empty)
        {
            _rows = [];
            return;
        }

        // fake/demo sample-data generation, never security-sensitive — System.Random is intentional
#pragma warning disable CA5394
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
                Mtime: baseTime + ((long)i * 10_000_000),
                Flags: (isDir ? 1u : 0u) | (isHiddenSystem ? 4u : 0u),
                Name: name,
                ParentPath: dirs[rng.Next(dirs.Length)]));
        }
#pragma warning restore CA5394
    }

    /// <summary>The shared syntax fixture: queries the real engine rejects.
    /// A missing/corrupt file degrades to accept-everything — loudly, never
    /// fatally (the fake must keep working in stripped-down test hosts).</summary>
    private static HashSet<string> LoadInvalidQueries()
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
                $"invalid_queries.json not loadable ({path}) — accepting every query",
                ex);
            return new HashSet<string>(StringComparer.Ordinal);
        }
    }

    /// <summary>Test hook: structurally invalidate every result handed out
    /// so far — their next GetRangeAsync throws <see cref="StaleResultException"/>,
    /// exactly like a real index rebuild (UI Stale→requery recovery).</summary>
    public void BumpEpoch() => Interlocked.Increment(ref _epoch);

    /// <inheritdoc/>
    public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default) =>
        Task.FromResult<IReadOnlyList<string>>(["F:"]);

    /// <inheritdoc/>
    public Task StartIndexingAsync(
        IReadOnlyList<string> volumes, CancellationToken ct = default) => Task.CompletedTask;

    /// <inheritdoc/>
    public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default) =>
        Task.FromResult<IReadOnlyList<VolumeStatus>>(
            [new("F:", VolumeState.Ready, (ulong)_rows.Count)]);

    private readonly List<QueryTraceData> _traces = [];

    /// <inheritdoc/>
    public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        var rows = (ulong)_rows.Count;
        var indexBytes = rows * 110;
        var stats = new EngineStatsData
        {
            RecentQueries = _traces.TakeLast(64).ToList(),
            P50Us = 1500,
            P90Us = 2500,
            P99Us = 4000,
            P999Us = 8000,
            QueryHistogram = new HistogramData
            {
                // bucket i covers [2^i, 2^(i+1)) µs; a small plausible spread.
                Buckets = [.. Enumerable.Range(0, 32).Select(i => i is 10 or 11 or 12 ? 4ul : 0ul)],
                Count = 12,
                SumUs = 30_000,
                MaxUs = 8_000,
            },
            Scans =
            [
                new ScanTraceData
                {
                    Volume = "F:",
                    Source = "snapshot",
                    ReadBytes = indexBytes,
                    ReadMs = 40,
                    MbPerS = 850,
                    ParseMs = 0,
                    DeferredMs = 0,
                    BuildMs = 30,
                    SortMs = 12,
                    TotalMs = 82,
                    Entries = rows,
                    PeakWsBytes = indexBytes + (96UL << 20),
                },
            ],
            Indexes =
            [
                new IndexStatsData
                {
                    Volume = "F:",
                    Entries = rows,
                    LiveEntries = rows,
                    TotalBytes = indexBytes,
                    BytesPerEntry = 110,
                    NamePoolBytes = rows * 40,
                    LowerPoolBytes = rows * 40,
                    OffsetsBytes = rows * 8,
                    ParentBytes = rows * 4,
                    SizeBytes = rows * 4,
                    MtimeBytes = rows * 4,
                    FrnBytes = rows * 8,
                    FlagBytes = rows,
                    PermutationsBytes = rows * 4,
                    FrnMapBytes = rows * 8,
                    DeadNameBytes = rows,
                    PoolGarbageRatio = 0.01,
                    DerivedCacheBytes = 0,
                    ContentGeneration = 3,
                    StructuralGeneration = 2,
                },
            ],
            CurrentWsBytes = indexBytes + (72UL << 20),
            CurrentPrivateBytes = indexBytes + (80UL << 20),
            RecentErrors = [.. _injectedErrors],
        };
        return Task.FromResult<EngineStatsData?>(stats);
    }

    /// <inheritdoc/>
    [System.Diagnostics.CodeAnalysis.SuppressMessage("Reliability", "CA2000:_", Justification = "ownership transferred to the caller (ISearchResult), disposed by the caller / on epoch change")]
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
        if (string.Equals(query.Trim(), "!!panic", StringComparison.Ordinal))
        {
            throw new EngineException("fault injection: simulated engine panic", 99);
        }

        if (string.Equals(query.Trim(), "!!warn", StringComparison.Ordinal))
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
            needle = needle.Replace("!!lag", string.Empty, StringComparison.Ordinal).Trim();
        }
#endif
        IEnumerable<RowData> hits;
        if (options.RegexMode && needle.Length != 0)
        {
            // Deterministic regex mode for UI tests. .NET regex differs from
            // the engine's rust regex, but the fake only needs to filter
            // plausibly and reject invalid patterns the same way (a 100ms
            // match timeout guards the test double against pathological input).
            var ci = options.Case switch
            {
                FmfCase.Insensitive => true,
                FmfCase.Sensitive => false,
                _ => !needle.Any(char.IsUpper),
            };
            System.Text.RegularExpressions.Regex re;
            var reOptions = ci
                ? System.Text.RegularExpressions.RegexOptions.IgnoreCase
                : System.Text.RegularExpressions.RegexOptions.None;
            try
            {
                re = new System.Text.RegularExpressions.Regex(
                    needle,
                    reOptions,
                    TimeSpan.FromMilliseconds(100));
            }
            catch (ArgumentException e)
            {
                throw new QuerySyntaxException($"invalid regex: {e.Message}");
            }

            hits = _rows.Where(r => re.IsMatch(options.Scope == RegexScope.Path ? r.FullPath : r.Name));
        }
        else
        {
            hits = needle.Length == 0
                ? _rows
                : _rows.Where(r => r.Name.Contains(needle, StringComparison.OrdinalIgnoreCase));
        }

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

    /// <inheritdoc/>
    public void Dispose()
    {
    }

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

        public void Dispose()
        {
        }
    }
}
