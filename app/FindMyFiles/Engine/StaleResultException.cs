namespace FindMyFiles.Engine;

/// <summary>The index was structurally rebuilt and the row IDs of a held result
/// handle are now invalid (thrown from
/// <see cref="ISearchResult.GetRangeAsync"/>). Recovery is to re-run the
/// query.</summary>
public sealed class StaleResultException : Exception
{
    /// <summary>Initializes with the canned message.</summary>
    public StaleResultException()
        : base("result is stale; re-run the query")
    {
    }
}
