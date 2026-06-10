using FindMyFiles.Engine;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// <see cref="IEngineClient"/> stub whose SearchAsync hands back
/// caller-controlled TaskCompletionSources — the test decides when (and in
/// which order) each query completes, which makes generation/supersede races
/// fully deterministic.
/// </summary>
public sealed class StubEngineClient : IEngineClient
{
    public sealed class PendingSearch(string query, SearchOptions options)
    {
        public string Query { get; } = query;
        public SearchOptions Options { get; } = options;
        public TaskCompletionSource<SearchOutcome> Tcs { get; } = new();

        /// <summary>Complete this query with rows; returns the result so the
        /// test can assert on its dispose state.</summary>
        public StubSearchResult CompleteWith(IReadOnlyList<RowData> rows, QueryTraceData? trace = null)
        {
            var result = new StubSearchResult(rows);
            CompleteWith(result, trace);
            return result;
        }

        public void CompleteWith(StubSearchResult result, QueryTraceData? trace = null) =>
            Tcs.SetResult(new SearchOutcome(result, trace));
    }

    /// <summary>Every SearchAsync call, in order, still awaiting completion
    /// until the test calls CompleteWith / sets the Tcs.</summary>
    public List<PendingSearch> Searches { get; } = [];

    /// <summary>When set, SearchAsync throws instead of recording a call.</summary>
    public Exception? ThrowOnSearch { get; set; }

    public event Action<string>? IndexChanged;
    public event Action<VolumeStatus>? VolumeUpdated { add { } remove { } }
    public event Action<int>? EngineErrorOccurred { add { } remove { } }

    public void RaiseIndexChanged(string volume) => IndexChanged?.Invoke(volume);

    public IReadOnlyList<string> ListVolumes() => ["F:"];

    public void StartIndexing(IReadOnlyList<string> volumes) { }

    public IReadOnlyList<VolumeStatus> GetStatus() => [];

    public Task<EngineStatsData?> GetStatsAsync() => Task.FromResult<EngineStatsData?>(null);

    public Task<SearchOutcome> SearchAsync(string query, SearchOptions options)
    {
        if (ThrowOnSearch is { } ex)
        {
            throw ex;
        }
        var pending = new PendingSearch(query, options);
        Searches.Add(pending);
        return pending.Tcs.Task;
    }

    public void Dispose() { }
}
