namespace FindMyFiles.Engine;

/// <summary>索引が構造的に再構築され、保持していた結果ハンドルの行 ID が
/// 無効になったことを示す(<see cref="ISearchResult.GetRangeAsync"/> から
/// 送出)。回復はクエリの再実行。</summary>
public sealed class StaleResultException : Exception
{
    /// <summary>定型メッセージで初期化する。</summary>
    public StaleResultException()
        : base("result is stale; re-run the query")
    {
    }
}
