namespace FindMyFiles.Engine;

/// <summary>Materialized, sort-ordered result; pages are O(1) reads.</summary>
public interface ISearchResult : IDisposable
{
    /// <summary>The settled total match count (the upper bound on rows obtainable
    /// via <see cref="GetRangeAsync"/>). This determines the virtualized list's
    /// scroll range.</summary>
    long Count { get; }

    /// <summary>Fetches up to <paramref name="count"/> rows from
    /// <paramref name="offset"/> in the result (page read of the visible
    /// window).</summary>
    /// <param name="offset">0-based row offset from the start.</param>
    /// <param name="count">Maximum number of rows to fetch.</param>
    /// <param name="ct">Cooperative cancellation token.</param>
    /// <returns>The <see cref="RowData"/> for the requested range (may be fewer
    /// than <paramref name="count"/> near the end).</returns>
    /// <exception cref="StaleResultException">
    /// The index was structurally rebuilt — re-run the query.
    /// </exception>
    Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default);
}
