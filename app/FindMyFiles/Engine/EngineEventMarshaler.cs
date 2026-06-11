using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// The single crossing point from engine threads to the UI thread.
/// <see cref="IEngineClient"/> events fire on engine / pipe read-loop
/// threads; this class subscribes to all four, marshals each through
/// <see cref="IDispatcher.TryEnqueue"/> and re-raises it as its own event on
/// the UI thread. Consumers (ViewModels, the orchestrator) attach plain
/// handlers and never marshal themselves — the per-subscriber inline
/// `dispatcher.TryEnqueue` lambdas this replaces were the audit's
/// "scattered crossing" finding. The upstream delegates are held in fields
/// for the subscription lifetime (the GC-rooting規約 for callback delegates,
/// satisfied structurally).
/// </summary>
public sealed class EngineEventMarshaler : IDisposable
{
    private readonly IEngineClient _engine;
    private readonly Action<string> _onIndexChanged;
    private readonly Action<VolumeStatus> _onVolumeUpdated;
    private readonly Action<int> _onEngineErrorOccurred;
    private readonly Action<EngineConnectionState> _onConnectionChanged;

    /// <summary>Re-raised on the UI thread. Same payloads, same relative
    /// order as the engine fired them (TryEnqueue is FIFO).</summary>
    public event Action<string>? IndexChanged;
    public event Action<VolumeStatus>? VolumeUpdated;
    public event Action<int>? EngineErrorOccurred;
    public event Action<EngineConnectionState>? ConnectionChanged;

    public EngineEventMarshaler(IEngineClient engine, IDispatcher dispatcher)
    {
        _engine = engine;
        _onIndexChanged = v => dispatcher.TryEnqueue(() => IndexChanged?.Invoke(v));
        _onVolumeUpdated = s => dispatcher.TryEnqueue(() => VolumeUpdated?.Invoke(s));
        _onEngineErrorOccurred = sev => dispatcher.TryEnqueue(() => EngineErrorOccurred?.Invoke(sev));
        _onConnectionChanged = s => dispatcher.TryEnqueue(() => ConnectionChanged?.Invoke(s));
        engine.IndexChanged += _onIndexChanged;
        engine.VolumeUpdated += _onVolumeUpdated;
        engine.EngineErrorOccurred += _onEngineErrorOccurred;
        engine.ConnectionChanged += _onConnectionChanged;
    }

    public void Dispose()
    {
        _engine.IndexChanged -= _onIndexChanged;
        _engine.VolumeUpdated -= _onVolumeUpdated;
        _engine.EngineErrorOccurred -= _onEngineErrorOccurred;
        _engine.ConnectionChanged -= _onConnectionChanged;
    }
}
