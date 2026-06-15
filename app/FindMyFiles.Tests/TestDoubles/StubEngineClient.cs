using FindMyFiles.Engine;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// <see cref="IEngineClient"/> stub whose SearchAsync hands back
/// caller-controlled TaskCompletionSources — the test decides when (and in
/// which order) each query completes, which makes generation/supersede races
/// fully deterministic.
///
/// Recording-only: this stub does NOT claim contract conformance and is
/// deliberately excluded from EngineClientContractTests — queries never fail
/// with QuerySyntaxException, nothing goes stale, cancellation is ignored.
/// Use <see cref="FindMyFiles.Engine.FakeEngineClient"/> when a test needs a
/// contract-conforming in-proc engine.
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

    /// <summary>When set, ListVolumesAsync faults — drives the StartAsync failure
    /// path (status + error notification) without a real engine.</summary>
    public Exception? ThrowOnStartup { get; set; }

    public event Action<string>? IndexChanged;
    public event Action<VolumeStatus>? VolumeUpdated;
    public event Action<int>? EngineErrorOccurred;
    public event Action<EngineConnectionState>? ConnectionChanged;

    public EngineConnectionState Connection => EngineConnectionState.InProc;

    public void RaiseIndexChanged(string volume) => IndexChanged?.Invoke(volume);

    public void RaiseVolumeUpdated(VolumeStatus status) => VolumeUpdated?.Invoke(status);

    public void RaiseEngineError(int severity) => EngineErrorOccurred?.Invoke(severity);

    public void RaiseConnectionChanged(EngineConnectionState state) =>
        ConnectionChanged?.Invoke(state);

    /// <summary>Live subscriber count per event — lets tests pin that
    /// Dispose paths actually unsubscribe.</summary>
    public int IndexChangedSubscribers => IndexChanged?.GetInvocationList().Length ?? 0;

    public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default) =>
        ThrowOnStartup is { } ex
            ? Task.FromException<IReadOnlyList<string>>(ex)
            : Task.FromResult<IReadOnlyList<string>>(["F:"]);

    public Task StartIndexingAsync(
        IReadOnlyList<string> volumes, CancellationToken ct = default) => Task.CompletedTask;

    public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default) =>
        Task.FromResult<IReadOnlyList<VolumeStatus>>([]);

    public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default) =>
        Task.FromResult<EngineStatsData?>(null);

    public Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default)
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
