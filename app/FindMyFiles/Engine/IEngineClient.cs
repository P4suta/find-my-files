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
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default);

    /// <summary>指定ボリュームの索引構築/再構築を開始するよう要求する
    /// (fire-and-trigger: 進捗は <see cref="VolumeUpdated"/> /
    /// <see cref="IndexChanged"/> で届く)。MFT 読みは昇格必須なので、実体は
    /// サービス側で走る。</summary>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task StartIndexingAsync(IReadOnlyList<string> volumes, CancellationToken ct = default);

    /// <summary>全ボリュームの現在状態(<see cref="VolumeStatus"/>)のスナップ
    /// ショットを返す。起動直後の初期表示や setup 画面の判定に使う。</summary>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default);

    /// <summary>クエリを実行し、ソート確定済みの結果ハンドル
    /// (<see cref="SearchOutcome.Result"/>)と任意の計測トレースを返す。
    /// ページ取得は返却された <see cref="ISearchResult"/> 経由で遅延読み。</summary>
    /// <exception cref="QuerySyntaxException">malformed query text</exception>
    /// <exception cref="EngineUnavailableException">service unreachable</exception>
    Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default);

    /// <summary>Observability snapshot for the performance panel.</summary>
    Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default);
}

/// <summary>Transport state of the engine boundary. In-proc clients are
/// always InProc; the pipe client reports its supervisor state.</summary>
public enum EngineConnectionState
{
    /// <summary>In-process engine (FFI): no transport, so always connected.</summary>
    InProc,
    /// <summary>The pipe client is establishing its first connection to the service.</summary>
    Connecting,
    /// <summary>The pipe client has a live connection to the service.</summary>
    Connected,
    /// <summary>The pipe connection dropped; the supervisor is re-establishing it.</summary>
    Reconnecting,
}

/// <summary>Materialized, sort-ordered result; pages are O(1) reads.</summary>
public interface ISearchResult : IDisposable
{
    /// <summary>確定した一致総数(<see cref="GetRangeAsync"/> で取れる行数の
    /// 上限)。仮想化リストのスクロール範囲はこれが決める。</summary>
    long Count { get; }

    /// <summary>結果の <paramref name="offset"/> から最大 <paramref name="count"/>
    /// 行を取得する(可視ウィンドウのページ読み)。</summary>
    /// <param name="offset">先頭からの 0 始まり行オフセット。</param>
    /// <param name="count">取得する最大行数。</param>
    /// <param name="ct">協調キャンセル用トークン。</param>
    /// <returns>要求範囲の <see cref="RowData"/>(末尾近くでは
    /// <paramref name="count"/> 未満になりうる)。</returns>
    /// <exception cref="StaleResultException">
    /// The index was structurally rebuilt — re-run the query.
    /// </exception>
    Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default);
}

/// <summary>索引が構造的に再構築され、保持していた結果ハンドルの行 ID が
/// 無効になったことを示す(<see cref="ISearchResult.GetRangeAsync"/> から
/// 送出)。回復はクエリの再実行。</summary>
public sealed class StaleResultException : Exception
{
    /// <summary>定型メッセージで初期化する。</summary>
    public StaleResultException() : base("result is stale; re-run the query") { }
}

/// <summary>クエリ文字列の構文が不正で <see cref="IEngineClient.SearchAsync"/>
/// がパースに失敗したことを示す。</summary>
/// <param name="message">パーサが返した人間可読な理由。</param>
public sealed class QuerySyntaxException(string message) : Exception(message);

/// <summary>The engine transport is down (pipe disconnected, request timed
/// out, service not running). Pending requests fail fast with this; the
/// supervisor keeps reconnecting in the background.</summary>
/// <param name="message">どの transport 障害かを示す人間可読な説明。</param>
public sealed class EngineUnavailableException(string message) : Exception(message);

/// <summary>エンジンが構造化エラーコード付きで操作を拒否した(transport は
/// 生きているがエンジンが失敗を返したケース)。コードは
/// docs/ARCHITECTURE.md の `FMF_E_*` 表に対応する。</summary>
/// <param name="message">エンジンが返した人間可読なメッセージ。</param>
/// <param name="code">`FMF_E_*` 数値コード(<see cref="Code"/> に保持)。</param>
public sealed class EngineException(string message, int code) : Exception(message)
{
    /// <summary>エンジンが返した `FMF_E_*` コード。UI 側の分岐
    /// (例: `FMF_E_LOCKED` で setup 画面へ)に使う。</summary>
    public int Code { get; } = code;
}
