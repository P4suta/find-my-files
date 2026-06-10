namespace FindMyFiles.Engine;

/// <summary>
/// Deterministic in-memory engine for UI tests and unelevated development
/// (`--fake-engine`). 100k entries from a fixed seed; substring search only.
/// </summary>
public sealed class FakeEngineClient : IEngineClient
{
    private const int EntryCount = 100_000;
    private readonly List<RowData> _rows;

    public event Action<string>? IndexChanged { add { } remove { } }
    public event Action<VolumeStatus>? VolumeUpdated { add { } remove { } }
    // Only the DEBUG-only fault injection raises this; Release builds keep
    // the member to satisfy IEngineClient.
#pragma warning disable CS0067
    public event Action<int>? EngineErrorOccurred;
#pragma warning restore CS0067

    private readonly List<ErrorEventData> _injectedErrors = [];

    public FakeEngineClient()
    {
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

    public IReadOnlyList<string> ListVolumes() => ["F:"];

    public void StartIndexing(IReadOnlyList<string> volumes) { }

    public IReadOnlyList<VolumeStatus> GetStatus() =>
        [new("F:", VolumeState.Ready, (ulong)_rows.Count)];

    private readonly List<QueryTraceData> _traces = [];

    public Task<EngineStatsData?> GetStatsAsync()
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

    public Task<SearchOutcome> SearchAsync(string query, SearchOptions options)
    {
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
        return Task.FromResult(new SearchOutcome(new FakeResult(list), trace));
    }

    public void Dispose() { }

    private sealed class FakeResult(List<RowData> rows) : ISearchResult
    {
        public long Count => rows.Count;

        public Task<IReadOnlyList<RowData>> GetRangeAsync(long offset, int count)
        {
            var start = (int)Math.Min(offset, rows.Count);
            var n = Math.Min(count, rows.Count - start);
            return Task.FromResult<IReadOnlyList<RowData>>(rows.GetRange(start, n));
        }

        public void Dispose() { }
    }
}
