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

    /// <summary>
    /// The engine recorded a diagnostic (1=warn 2=error 3=panic). Details
    /// live in <see cref="EngineStatsData.RecentErrors"/> — pull on demand.
    /// </summary>
    event Action<int>? EngineErrorOccurred;

    IReadOnlyList<string> ListVolumes();
    void StartIndexing(IReadOnlyList<string> volumes);
    IReadOnlyList<VolumeStatus> GetStatus();

    /// <exception cref="QuerySyntaxException">malformed query text</exception>
    Task<SearchOutcome> SearchAsync(string query, SearchOptions options);

    /// <summary>Observability snapshot for the performance panel.</summary>
    Task<EngineStatsData?> GetStatsAsync();
}

public sealed record SearchOutcome(ISearchResult Result, QueryTraceData? Trace);

/// <summary>Stage breakdown of one query (mirrors fmf-core metrics.rs).</summary>
public sealed class QueryTraceData
{
    public string Query { get; set; } = string.Empty;
    public string Driver { get; set; } = string.Empty;
    public ulong ParseUs { get; set; }
    public ulong CompileUs { get; set; }
    public ulong MemoUs { get; set; }
    public ulong ScanUs { get; set; }
    public ulong MaterializeUs { get; set; }
    public ulong MergeUs { get; set; }
    public ulong TotalUs { get; set; }
    public ulong EntriesScanned { get; set; }
    public ulong ExcludedSkipped { get; set; }
    public ulong Hits { get; set; }
    public uint Volumes { get; set; }

    /// <summary>Engine-verified: same query as last time with identical id
    /// lists on every volume — the screen has nothing to change.</summary>
    public bool Unchanged { get; set; }
}

public sealed class IndexStatsData
{
    public string Volume { get; set; } = string.Empty;
    public ulong Entries { get; set; }
    public ulong LiveEntries { get; set; }
    public ulong Tombstones { get; set; }
    public ulong TotalBytes { get; set; }
    public double BytesPerEntry { get; set; }
    public ulong ContentGeneration { get; set; }
}

public sealed class UsnTraceData
{
    public string Volume { get; set; } = string.Empty;
    public ulong Records { get; set; }
    public ulong Upserted { get; set; }
    public ulong Deleted { get; set; }
    public ulong StatUpdated { get; set; }
    public ulong ApplyUs { get; set; }
}

public sealed class ErrorEventData
{
    public ulong Seq { get; set; }
    public ulong UptimeMs { get; set; }
    public string Severity { get; set; } = string.Empty; // warn|error|panic
    public string Area { get; set; } = string.Empty;
    public string? Volume { get; set; }
    public string Message { get; set; } = string.Empty;
}

public sealed class CountersData
{
    public ulong StatFetchFailures { get; set; }
    public ulong UsnBatchesTruncated { get; set; }
    public ulong SnapshotLoadFailures { get; set; }
    public ulong SnapshotSaveFailures { get; set; }
    public ulong DeferredNamesUnresolved { get; set; }
    public ulong CorruptMftRecords { get; set; }
    public ulong JournalRescans { get; set; }
}

public sealed class EngineStatsData
{
    public List<QueryTraceData> RecentQueries { get; set; } = [];
    public ulong P50Us { get; set; }
    public ulong P99Us { get; set; }
    public List<UsnTraceData> RecentUsn { get; set; } = [];
    public List<IndexStatsData> Indexes { get; set; } = [];
    public CountersData Counters { get; set; } = new();
    public List<ErrorEventData> RecentErrors { get; set; } = [];
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
