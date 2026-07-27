namespace FindMyFiles.Engine;

/// <summary>
/// Linearizable admission gate shared by page fetches and presentation-basis
/// use. Disposal closes admission first and claims the one wire release only
/// after all already-admitted operations drain.
/// </summary>
internal sealed class ResultLeaseGate
{
    private readonly System.Threading.Lock _gate = new();
    private int _active;
    private bool _disposed;
    private bool _releaseClaimed;

    /// <summary>Attempts to admit one operation before disposal closes the gate.</summary>
    /// <returns>True when the caller owns a lease that it must release.</returns>
    internal bool TryAcquire()
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return false;
            }

            _active++;
            return true;
        }
    }

    /// <summary>Returns one admitted operation to the gate.</summary>
    /// <returns>True exactly once when the caller must release the result.</returns>
    internal bool Release()
    {
        lock (_gate)
        {
            if (_active <= 0)
            {
                throw new InvalidOperationException("result lease underflow");
            }

            _active--;
            return ClaimReleaseIfReady();
        }
    }

    /// <summary>Closes admission and marks the result for deferred release.</summary>
    /// <returns>True exactly once when the caller must release the result.</returns>
    internal bool Dispose()
    {
        lock (_gate)
        {
            _disposed = true;
            return ClaimReleaseIfReady();
        }
    }

    private bool ClaimReleaseIfReady()
    {
        if (!_disposed || _active != 0 || _releaseClaimed)
        {
            return false;
        }

        _releaseClaimed = true;
        return true;
    }
}
