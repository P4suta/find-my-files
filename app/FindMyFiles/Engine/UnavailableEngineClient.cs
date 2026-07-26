namespace FindMyFiles.Engine;

/// <summary>
/// Explicit no-engine state used when the service is not installed/reachable
/// or engine initialization failed. It contains no demo data and performs no
/// privileged work; the main page recognizes <see cref="Kind"/> and shows the
/// setup/recovery experience instead of issuing calls to it.
/// </summary>
internal sealed class UnavailableEngineClient : IEngineClient
{
    /// <inheritdoc/>
    public EngineClientKind Kind => EngineClientKind.Unavailable;

    /// <inheritdoc/>
    public EngineConnectionState Connection => EngineConnectionState.InProc;

    /// <inheritdoc/>
    public event Action<string>? IndexChanged
    {
        add { }
        remove { }
    }

    /// <inheritdoc/>
    public event Action<VolumeStatus>? VolumeUpdated
    {
        add { }
        remove { }
    }

    /// <inheritdoc/>
    public event Action<int>? EngineErrorOccurred
    {
        add { }
        remove { }
    }

    /// <inheritdoc/>
    public event Action<EngineConnectionState>? ConnectionChanged
    {
        add { }
        remove { }
    }

    /// <inheritdoc/>
    public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        return Task.FromResult<IReadOnlyList<string>>([]);
    }

    /// <inheritdoc/>
    public Task StartIndexingAsync(
        IReadOnlyList<string> volumes,
        CancellationToken ct = default)
    {
        _ = EngineRequest.Volumes(volumes);
        ct.ThrowIfCancellationRequested();
        return Task.CompletedTask;
    }

    /// <inheritdoc/>
    public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        return Task.FromResult<IReadOnlyList<VolumeStatus>>([]);
    }

    /// <inheritdoc/>
    public Task<SearchOutcome> SearchAsync(
        string query,
        SearchOptions options,
        CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        return Task.FromException<SearchOutcome>(
            new EngineUnavailableException("engine is unavailable"));
    }

    /// <inheritdoc/>
    public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        return Task.FromResult<EngineStatsData?>(null);
    }

    /// <inheritdoc/>
    public void Dispose()
    {
    }
}
