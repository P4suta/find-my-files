namespace FindMyFiles.Engine;

/// <summary>
/// The single boundary the app uses to talk to the engine
/// (docs/ARCHITECTURE.md). Implementations: <see cref="PipeEngineClient"/>
/// (named pipe to fmf-service), <see cref="FfiEngineClient"/> (in-proc DLL,
/// --engine=inproc) and <see cref="FakeEngineClient"/> (deterministic data
/// for UI tests via --fake-engine). The shared observable behavior is
/// executable: Tests/Contract/EngineClientContractTests runs the same suite
/// against all implementations.
///
/// Cancellation is cooperative and fully plumbed (ADR-0018): a cancelled
/// <c>ct</c> surfaces as <see cref="OperationCanceledException"/> (or a
/// subclass) from every async member. Data shapes live in EngineTypes.cs.
/// </summary>
public interface IEngineClient : IDisposable
{
    /// <summary>新しい索引内容が公開された(USN 適用やスキャン進捗)。
    /// payload はトリガとなったボリュームラベル。表示中のクエリを再評価する
    /// 合図。engine スレッドから発火するので UI スレッドへ marshal すること
    /// (<see cref="EngineEventMarshaler"/> が唯一の交差点)。</summary>
    event Action<string>? IndexChanged;

    /// <summary>1 ボリュームの状態が遷移した(`Scanning`→`Ready` 等)。最新の
    /// <see cref="VolumeStatus"/> を払い出す。engine スレッド発火 → 要 marshal。</summary>
    event Action<VolumeStatus>? VolumeUpdated;

    /// <summary>
    /// The engine recorded a diagnostic (1=warn 2=error 3=panic). Details
    /// live in <see cref="EngineStatsData.RecentErrors"/> — pull on demand.
    /// </summary>
    event Action<int>? EngineErrorOccurred;

    /// <summary>InProc for Ffi/Fake (fixed, never raises
    /// <see cref="ConnectionChanged"/>); the pipe client moves through
    /// Connecting/Connected/Reconnecting.</summary>
    EngineConnectionState Connection { get; }

    /// <summary>現在の <see cref="Connection"/> が遷移したときに発火。in-proc
    /// 実装は一度も発火しない(常に <see cref="EngineConnectionState.InProc"/>)。
    /// pipe クライアントの supervisor スレッド発火 → 要 marshal。</summary>
    event Action<EngineConnectionState>? ConnectionChanged;

    /// <summary>索引済みボリュームのラベル一覧を返す(現在エンジンが
    /// 認識しているもの)。</summary>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>The labels of the volumes the engine currently knows about.</returns>
    Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default);

    /// <summary>指定ボリュームの索引構築/再構築を開始するよう要求する
    /// (fire-and-trigger: 進捗は <see cref="VolumeUpdated"/> /
    /// <see cref="IndexChanged"/> で届く)。MFT 読みは昇格必須なので、実体は
    /// サービス側で走る。</summary>
    /// <param name="volumes">Labels of the volumes to (re)index.</param>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>A task that completes once the indexing request is accepted.</returns>
    Task StartIndexingAsync(IReadOnlyList<string> volumes, CancellationToken ct = default);

    /// <summary>全ボリュームの現在状態(<see cref="VolumeStatus"/>)のスナップ
    /// ショットを返す。起動直後の初期表示や setup 画面の判定に使う。</summary>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>A snapshot of the current status of every known volume.</returns>
    Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default);

    /// <summary>クエリを実行し、ソート確定済みの結果ハンドル
    /// (<see cref="SearchOutcome.Result"/>)と任意の計測トレースを返す。
    /// ページ取得は返却された <see cref="ISearchResult"/> 経由で遅延読み。</summary>
    /// <param name="query">The query text to execute.</param>
    /// <param name="options">Search options (sort order, flags).</param>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <exception cref="QuerySyntaxException">malformed query text</exception>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    /// <returns>The sort-ordered result handle plus an optional timing trace.</returns>
    Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default);

    /// <summary>Observability snapshot for the performance panel.</summary>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <returns>The current engine stats, or <c>null</c> if unavailable.</returns>
    Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default);
}
