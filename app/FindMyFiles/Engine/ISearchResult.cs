namespace FindMyFiles.Engine;

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
