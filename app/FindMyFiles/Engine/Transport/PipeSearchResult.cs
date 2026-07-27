namespace FindMyFiles.Engine;

/// <summary>
/// Pipe-backed <see cref="ISearchResult"/>. Pages stale out when the
/// connection epoch moves (disconnects); Dispose defers the wire-level
/// ResultFree until every in-flight page fetch has drained.
/// </summary>
internal sealed class PipeSearchResult(
    PipeEngineClient client, ulong resultId, long count, int epoch) : ISearchResult
{
    private readonly ResultLeaseGate _lifetime = new();

    public long Count { get; } = count;

    internal bool TryAcquirePresentationBasis(
        PipeEngineClient expectedClient,
        out ulong id,
        out int basisEpoch,
        out IDisposable? lease)
    {
        id = 0;
        basisEpoch = 0;
        lease = null;
        if (!ReferenceEquals(client, expectedClient)
            || epoch != client.CurrentEpoch
            || !_lifetime.TryAcquire())
        {
            return false;
        }

        if (epoch != client.CurrentEpoch)
        {
            EndOperation();
            return false;
        }

        id = resultId;
        basisEpoch = epoch;
        lease = new OperationLease(EndOperation);
        return true;
    }

    public async Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        var request = EngineRequest.PageRange(offset, count);
        if (epoch != client.CurrentEpoch || !_lifetime.TryAcquire())
        {
            throw new StaleResultException();
        }

        try
        {
            if (epoch != client.CurrentEpoch)
            {
                throw new StaleResultException(); // re-check inside the guard
            }

            return await client.FetchPageAsync(resultId, request, epoch, ct).ConfigureAwait(false);
        }
        finally
        {
            EndOperation();
        }
    }

    public void Dispose()
    {
        if (_lifetime.Dispose())
        {
            client.ReleaseResult(resultId, epoch);
        }
    }

    private void EndOperation()
    {
        if (_lifetime.Release())
        {
            client.ReleaseResult(resultId, epoch);
        }
    }

    private sealed class OperationLease(Action release) : IDisposable
    {
        private Action? _release = release;

        public void Dispose() =>
            Interlocked.Exchange(ref _release, null)?.Invoke();
    }
}
