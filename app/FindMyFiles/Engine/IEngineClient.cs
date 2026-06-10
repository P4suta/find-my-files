namespace FindMyFiles.Engine;

/// <summary>
/// The single boundary the app uses to talk to the engine
/// (docs/ARCHITECTURE.md). Implementations: <see cref="FfiEngineClient"/>
/// (in-proc DLL, MVP), <see cref="FakeEngineClient"/> (deterministic data for
/// UI tests via --fake-engine), and a future named-pipe client (v2 service).
/// </summary>
public interface IEngineClient : IDisposable
{
    /// <summary>Raised from engine threads (marshal to the UI thread).</summary>
    event Action<string>? IndexChanged;
    event Action<VolumeStatus>? VolumeUpdated;

    IReadOnlyList<string> ListVolumes();
    void StartIndexing(IReadOnlyList<string> volumes);
    IReadOnlyList<VolumeStatus> GetStatus();

    /// <exception cref="QuerySyntaxException">malformed query text</exception>
    Task<ISearchResult> SearchAsync(string query, SearchOptions options);
}

public enum FmfSort { Name = 0, Size = 1, Mtime = 2 }

public enum FmfCase { Smart = 0, Insensitive = 1, Sensitive = 2 }

public sealed record SearchOptions(
    FmfSort Sort,
    bool Descending,
    FmfCase Case,
    bool IncludeHiddenSystem = false)
{
    public static readonly SearchOptions Default = new(FmfSort.Name, false, FmfCase.Smart);
}

public enum VolumeState { Scanning = 0, Ready = 1, Rescanning = 2, Failed = 3 }

public sealed record VolumeStatus(string Label, VolumeState State, ulong Entries);

public sealed record RowData(
    ulong EntryRef,
    ulong Frn,
    ulong Size,
    long Mtime,
    uint Flags,
    string Name,
    string ParentPath)
{
    public bool IsDirectory => (Flags & 1) != 0;
    public string FullPath => ParentPath + Name;
}

/// <summary>Materialized, sort-ordered result; pages are O(1) reads.</summary>
public interface ISearchResult : IDisposable
{
    long Count { get; }

    /// <exception cref="StaleResultException">
    /// The index was structurally rebuilt — re-run the query.
    /// </exception>
    Task<IReadOnlyList<RowData>> GetRangeAsync(long offset, int count);
}

public sealed class StaleResultException : Exception
{
    public StaleResultException() : base("result is stale; re-run the query") { }
}

public sealed class QuerySyntaxException(string message) : Exception(message);

public sealed class EngineException(string message, int code) : Exception(message)
{
    public int Code { get; } = code;
}
