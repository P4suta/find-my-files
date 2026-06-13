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

    /// <summary>UI スレッドで再発火する <see cref="IEngineClient.IndexChanged"/>。
    /// payload も engine が発火した相対順序もそのまま(TryEnqueue は FIFO)。</summary>
    public event Action<string>? IndexChanged;

    /// <summary>UI スレッドで再発火する <see cref="IEngineClient.VolumeUpdated"/>
    /// (同 payload・同順序)。</summary>
    public event Action<VolumeStatus>? VolumeUpdated;

    /// <summary>UI スレッドで再発火する
    /// <see cref="IEngineClient.EngineErrorOccurred"/>(同 severity・同順序)。</summary>
    public event Action<int>? EngineErrorOccurred;

    /// <summary>UI スレッドで再発火する
    /// <see cref="IEngineClient.ConnectionChanged"/>(同 payload・同順序)。</summary>
    public event Action<EngineConnectionState>? ConnectionChanged;

    /// <summary><paramref name="engine"/> の 4 つのイベントを購読し、各 payload を
    /// <paramref name="dispatcher"/> 経由で UI スレッドへ marshal して、本クラスの
    /// 同名イベントとして再発火するよう結線する。upstream delegate はフィールドに
    /// 保持され(購読寿命=GC ルート)、<see cref="Dispose"/> で解除される。</summary>
    /// <param name="engine">購読元のエンジンクライアント。</param>
    /// <param name="dispatcher">UI スレッドへの marshal 先。</param>
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

    /// <summary>4 つの upstream 購読をすべて解除する(以降このマーシャラは
    /// イベントを再発火しない)。</summary>
    public void Dispose()
    {
        _engine.IndexChanged -= _onIndexChanged;
        _engine.VolumeUpdated -= _onVolumeUpdated;
        _engine.EngineErrorOccurred -= _onEngineErrorOccurred;
        _engine.ConnectionChanged -= _onConnectionChanged;
    }
}
