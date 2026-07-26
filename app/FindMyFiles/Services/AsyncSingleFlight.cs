namespace FindMyFiles.Services;

/// <summary>Runs at most one asynchronous refresh at a time. Timer ticks that
/// arrive while work is in flight are coalesced instead of stacking engine
/// requests. Disposal prevents new work after the owning view is torn down.</summary>
internal sealed class AsyncSingleFlight : IDisposable
{
    private int _running;
    private int _disposed;

    /// <summary>Run <paramref name="action"/> unless another call is active or
    /// this gate has been disposed.</summary>
    /// <param name="action">Asynchronous refresh to run at most once concurrently.</param>
    /// <returns>A task that completes with the accepted action, or immediately
    /// when the call was coalesced.</returns>
    public async Task RunAsync(Func<Task> action)
    {
        ArgumentNullException.ThrowIfNull(action);
        if (Volatile.Read(ref _disposed) != 0
            || Interlocked.CompareExchange(ref _running, 1, 0) != 0)
        {
            return;
        }

        try
        {
            if (Volatile.Read(ref _disposed) == 0)
            {
                await action().ConfigureAwait(true);
            }
        }
        finally
        {
            Volatile.Write(ref _running, 0);
        }
    }

    /// <summary>Reject all future refreshes. In-flight work owns its own
    /// cancellation token and is allowed to unwind normally.</summary>
    public void Dispose() => Interlocked.Exchange(ref _disposed, 1);
}
