namespace FindMyFiles.Engine;

/// <summary>
/// Pipe-backed <see cref="ISearchResult"/>. Pages stale out when the
/// connection epoch moves (disconnects); Dispose defers the wire-level
/// ResultFree until every in-flight page fetch has drained.
/// </summary>
internal sealed class PipeSearchResult(
    PipeEngineClient client, ulong resultId, long count, int epoch) : ISearchResult
{
    private int _inFlight;
    private int _released;
    private volatile bool _disposed;

    public long Count { get; } = count;

    public async Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default)
    {
        if (_disposed || epoch != client.CurrentEpoch)
        {
            throw new StaleResultException();
        }
        Interlocked.Increment(ref _inFlight);
        try
        {
            if (epoch != client.CurrentEpoch)
            {
                throw new StaleResultException(); // re-check inside the guard
            }
            return await client.FetchPageAsync(resultId, offset, count, ct).ConfigureAwait(false);
        }
        finally
        {
            if (Interlocked.Decrement(ref _inFlight) == 0 && _disposed)
            {
                MaybeRelease();
            }
        }
    }

    public void Dispose()
    {
        _disposed = true;
        if (Volatile.Read(ref _inFlight) == 0)
        {
            MaybeRelease();
        }
    }

    private void MaybeRelease()
    {
        if (Interlocked.Exchange(ref _released, 1) == 0)
        {
            client.ReleaseResult(resultId, epoch);
        }
    }
}
