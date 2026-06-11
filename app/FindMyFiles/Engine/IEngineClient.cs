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
    /// <summary>Raised from engine threads (marshal to the UI thread).</summary>
    event Action<string>? IndexChanged;
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
    event Action<EngineConnectionState>? ConnectionChanged;

    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default);

    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task StartIndexingAsync(IReadOnlyList<string> volumes, CancellationToken ct = default);

    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default);

    /// <exception cref="QuerySyntaxException">malformed query text</exception>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default);

    /// <summary>Observability snapshot for the performance panel.</summary>
    Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default);
}

/// <summary>Transport state of the engine boundary. In-proc clients are
/// always InProc; the pipe client reports its supervisor state.</summary>
public enum EngineConnectionState { InProc, Connecting, Connected, Reconnecting }

/// <summary>Materialized, sort-ordered result; pages are O(1) reads.</summary>
public interface ISearchResult : IDisposable
{
    long Count { get; }

    /// <exception cref="StaleResultException">
    /// The index was structurally rebuilt — re-run the query.
    /// </exception>
    Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default);
}

public sealed class StaleResultException : Exception
{
    public StaleResultException() : base("result is stale; re-run the query") { }
}

public sealed class QuerySyntaxException(string message) : Exception(message);

/// <summary>The engine transport is down (pipe disconnected, request timed
/// out, service not running). Pending requests fail fast with this; the
/// supervisor keeps reconnecting in the background.</summary>
public sealed class EngineUnavailableException(string message) : Exception(message);

public sealed class EngineException(string message, int code) : Exception(message)
{
    public int Code { get; } = code;
}
