namespace FindMyFiles.Engine;

/// <summary>
/// The single boundary the app uses to talk to the engine
/// (docs/ARCHITECTURE.md). Implementations: <see cref="PipeEngineClient"/>
/// (named pipe to fmf-service), <see cref="FfiEngineClient"/> (in-proc DLL,
/// --engine=inproc) and <see cref="FakeEngineClient"/> (deterministic data
/// for UI tests via --fake-engine). The shared observable behavior is
/// executable: Tests/Contract/EngineClientContractTests runs the same suite
/// against all implementations.
///
/// Cancellation is cooperative and fully plumbed (ADR-0018): a cancelled
/// <c>ct</c> surfaces as <see cref="OperationCanceledException"/> (or a
/// subclass) from every async member. Data shapes live in EngineTypes.cs.
/// </summary>
public interface IEngineClient : IDisposable
{
    /// <summary>New index content was published (USN apply or scan progress).
    /// The payload is the triggering volume label. The signal to re-evaluate the
    /// displayed query. Fires from an engine thread, so marshal to the UI thread
    /// (<see cref="EngineEventMarshaler"/> is the only crossing point).</summary>
    event Action<string>? IndexChanged;

    /// <summary>One volume's state transitioned (`Scanning`→`Ready` etc.). Emits
    /// the latest <see cref="VolumeStatus"/>. Fires from an engine thread → marshal.</summary>
    event Action<VolumeStatus>? VolumeUpdated;

    /// <summary>
    /// The engine recorded a diagnostic (1=warn 2=error 3=panic). Details
    /// live in <see cref="EngineStatsData.RecentErrors"/> — pull on demand.
    /// </summary>
    event Action<int>? EngineErrorOccurred;

    /// <summary>InProc for Ffi/Fake (fixed, never raises
    /// <see cref="ConnectionChanged"/>); the pipe client moves through
    /// Connecting/Connected/Reconnecting.</summary>
    EngineConnectionState Connection { get; }

    /// <summary>Fires when the current <see cref="Connection"/> transitions. In-proc
    /// implementations never fire (always <see cref="EngineConnectionState.InProc"/>).
    /// Fires from the pipe client's supervisor thread → marshal.</summary>
    event Action<EngineConnectionState>? ConnectionChanged;

    /// <summary>Returns the labels of the indexed volumes (those the engine
    /// currently recognizes).</summary>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>The labels of the volumes the engine currently knows about.</returns>
    Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default);

    /// <summary>Requests that indexing/re-indexing start for the given volumes
    /// (fire-and-trigger: progress arrives via <see cref="VolumeUpdated"/> /
    /// <see cref="IndexChanged"/>). MFT reads require elevation, so the work runs
    /// on the service side.</summary>
    /// <param name="volumes">Labels of the volumes to (re)index.</param>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>A task that completes once the indexing request is accepted.</returns>
    Task StartIndexingAsync(IReadOnlyList<string> volumes, CancellationToken ct = default);

    /// <summary>Returns a snapshot of the current state
    /// (<see cref="VolumeStatus"/>) of every volume. Used for the initial display
    /// at startup and for the setup screen's decisions.</summary>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>A snapshot of the current status of every known volume.</returns>
    Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default);

    /// <summary>Executes the query and returns a sort-settled result handle
    /// (<see cref="SearchOutcome.Result"/>) plus an optional timing trace.
    /// Pages are read lazily through the returned <see cref="ISearchResult"/>.</summary>
    /// <param name="query">The query text to execute.</param>
    /// <param name="options">Search options (sort order, flags).</param>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="QuerySyntaxException">malformed query text</exception>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>The sort-ordered result handle plus an optional timing trace.</returns>
    Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default);

    /// <summary>Observability snapshot for the performance panel.</summary>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <returns>The current engine stats, or <c>null</c> if unavailable.</returns>
    Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default);
}
