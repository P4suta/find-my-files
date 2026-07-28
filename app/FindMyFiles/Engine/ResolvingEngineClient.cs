namespace FindMyFiles.Engine;

/// <summary>
/// Empty, non-privileged placeholder used only while startup resolves the real
/// engine off the UI thread. Its Connecting state keeps recovery/search actions
/// disabled without falsely presenting the service as absent.
/// </summary>
internal sealed class ResolvingEngineClient : IEngineClient
{
    public EngineClientKind Kind => EngineClientKind.Resolving;

    public EngineConnectionState Connection => EngineConnectionState.Connecting;

    public event Action<string>? IndexChanged
    {
        add { }
        remove { }
    }

    public event Action<VolumeStatus>? VolumeUpdated
    {
        add { }
        remove { }
    }

    public event Action<EngineErrorSeverity>? EngineErrorOccurred
    {
        add { }
        remove { }
    }

    public event Action<EngineConnectionState>? ConnectionChanged
    {
        add { }
        remove { }
    }

    public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default) =>
        Unavailable<IReadOnlyList<string>>(ct);

    public Task StartIndexingAsync(
        IReadOnlyList<string> volumes,
        CancellationToken ct = default)
    {
        _ = EngineRequest.Volumes(volumes);
        return Unavailable(ct);
    }

    public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default) =>
        Unavailable<IReadOnlyList<VolumeStatus>>(ct);

    public Task<SearchOutcome> SearchAsync(
        string query,
        SearchOptions options,
        CancellationToken ct = default) =>
        Unavailable<SearchOutcome>(ct);

    public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        return Task.FromResult<EngineStatsData?>(null);
    }

    public void Dispose()
    {
    }

    private static Task Unavailable(CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        return Task.FromException(new EngineUnavailableException(
            "engine resolution is still in progress"));
    }

    private static Task<T> Unavailable<T>(CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        return Task.FromException<T>(new EngineUnavailableException(
            "engine resolution is still in progress"));
    }
}
