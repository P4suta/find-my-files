using FindMyFiles.Engine;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// <see cref="ISearchResult"/> test double. By default pages resolve
/// synchronously; <see cref="Gate"/> holds every fetch until the test
/// releases it (for in-flight/epoch races), and <see cref="ThrowOnFetch"/>
/// faults the fetch instead of returning rows.
/// </summary>
public sealed class StubSearchResult(IReadOnlyList<RowData> rows) : ISearchResult
{
    public int FetchCount { get; private set; }

    public int DisposeCount { get; private set; }

    public bool Disposed => DisposeCount > 0;

    /// <summary>When set, GetRangeAsync awaits this before completing.</summary>
    public TaskCompletionSource? Gate { get; init; }

    /// <summary>When set, GetRangeAsync throws after passing the gate.</summary>
    public Exception? ThrowOnFetch { get; init; }

    /// <summary>When true, GetRangeAsync honors a cancelled ct after passing
    /// the gate (OperationCanceledException) — pins the cancellation defense.
    /// Default false: the fetch completes with data even when cancelled,
    /// pinning the epoch defense on its own (the two defenses are pinned by
    /// separate tests on purpose).</summary>
    public bool HonorCancellation { get; init; }

    /// <summary>The ct of the most recent GetRangeAsync call — lets tests
    /// assert the token was actually wired through and cancelled.</summary>
    public CancellationToken LastFetchToken { get; private set; }

    public long Count => rows.Count;

    public async Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default)
    {
        FetchCount++;
        LastFetchToken = ct;
        if (Gate is { } gate)
        {
            await gate.Task;
        }

        if (HonorCancellation)
        {
            ct.ThrowIfCancellationRequested();
        }

        if (ThrowOnFetch is { } ex)
        {
            throw ex;
        }

        var start = (int)Math.Min(offset, rows.Count);
        var n = Math.Max(0, Math.Min(count, rows.Count - start));
        return [.. rows.Skip(start).Take(n)];
    }

    public void Dispose() => DisposeCount++;
}
