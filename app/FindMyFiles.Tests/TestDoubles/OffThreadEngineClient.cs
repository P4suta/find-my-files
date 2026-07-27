using FindMyFiles.Engine;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// An <see cref="IEngineClient"/> whose every async member <em>really</em>
/// suspends and completes on a thread-pool thread — the engine half of the
/// UI-thread-affinity harness (<see cref="DedicatedThreadDispatcher"/> is the
/// other half).
///
/// <para>This is what makes a stray <c>ConfigureAwait(false)</c> observable.
/// A double that hands back an already-completed task resumes its awaiter
/// inline on the calling (UI) thread, so the continuation lands on the UI
/// thread <em>whether or not</em> the context was captured, and the bug class
/// that ships RPC_E_WRONG_THREAD crashes stays invisible. Every method here
/// therefore awaits a real delay with <c>ConfigureAwait(false)</c> first: the
/// caller's continuation then runs on the UI thread only if the caller
/// captured its dispatcher.</para>
///
/// <para>Recording-only, like <see cref="StubEngineClient"/>: it does not
/// claim <c>IEngineClient</c> contract conformance (queries never fail, nothing
/// goes stale) and is not part of EngineClientContractTests.</para>
/// </summary>
internal sealed class OffThreadEngineClient : IEngineClient
{
    /// <summary>Suspension long enough that the TPL cannot complete the task
    /// synchronously, short enough to keep the suite fast.</summary>
    private static readonly TimeSpan Hop = TimeSpan.FromMilliseconds(15);

    private readonly List<RowData> _rows;

    /// <summary>Builds a client that answers every query with
    /// <paramref name="rowCount"/> deterministic rows.</summary>
    /// <param name="rowCount">Rows the single canned result contains.</param>
    public OffThreadEngineClient(int rowCount = 0)
    {
        _rows = Rows.Many(rowCount);
    }

    public EngineClientKind Kind { get; set; } = EngineClientKind.InProcess;

    /// <summary>When set, the startup trio (<see cref="ListVolumesAsync"/>)
    /// faults <em>after</em> hopping off the UI thread — the failure path that
    /// writes bound status text and pushes a notification.</summary>
    public Exception? ThrowOnStartup { get; set; }

    /// <summary>When set, <see cref="GetStatsAsync"/> faults after the hop.</summary>
    public Exception? ThrowOnStats { get; set; }

    /// <summary>Stats snapshot handed back once the hop completes.</summary>
    public EngineStatsData? Stats { get; set; }

#pragma warning disable CS0067 // part of the interface; this double never raises them
    public event Action<string>? IndexChanged;

    public event Action<VolumeStatus>? VolumeUpdated;

    public event Action<EngineErrorSeverity>? EngineErrorOccurred;

    public event Action<EngineConnectionState>? ConnectionChanged;
#pragma warning restore CS0067

    public EngineConnectionState Connection { get; set; } = EngineConnectionState.InProc;

    public async Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default)
    {
        await Task.Delay(Hop, ct).ConfigureAwait(false);
        return ThrowOnStartup is { } ex ? throw ex : (IReadOnlyList<string>)["F:"];
    }

    public async Task StartIndexingAsync(
        IReadOnlyList<string> volumes, CancellationToken ct = default) =>
        await Task.Delay(Hop, ct).ConfigureAwait(false);

    public async Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default)
    {
        await Task.Delay(Hop, ct).ConfigureAwait(false);
        return [];
    }

    public async Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default)
    {
        await Task.Delay(Hop, ct).ConfigureAwait(false);
        return new SearchOutcome(
            new OffThreadSearchResult(_rows),
            new QueryTraceData { TotalUs = 42, Hits = (ulong)_rows.Count });
    }

    public async Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        await Task.Delay(Hop, ct).ConfigureAwait(false);
        return ThrowOnStats is { } ex ? throw ex : Stats;
    }

    public void Dispose()
    {
    }

    /// <summary>Result handle whose page reads also complete on a pool thread,
    /// so the presenter's prefetch awaits are exercised the same way.</summary>
    private sealed class OffThreadSearchResult(List<RowData> rows) : ISearchResult
    {
        public long Count => rows.Count;

        public async Task<IReadOnlyList<RowData>> GetRangeAsync(
            long offset, int count, CancellationToken ct = default)
        {
            await Task.Delay(Hop, ct).ConfigureAwait(false);
            var start = (int)Math.Min(offset, rows.Count);
            var n = Math.Min(count, rows.Count - start);
            return rows.GetRange(start, n);
        }

        public void Dispose()
        {
        }
    }
}
