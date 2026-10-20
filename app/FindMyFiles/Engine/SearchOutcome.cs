namespace FindMyFiles.Engine;

/// <summary>What <see cref="IEngineClient.SearchAsync(string, SearchOptions, CancellationToken)"/> returns: the materialized <see cref="ISearchResult"/> the UI pages through, paired with the per-query <see cref="QueryTraceData"/> the engine attached (null when tracing was unavailable, e.g. a serialization failure — the result is still valid).</summary> <param name="Result">The sort-ordered, O(1)-paged result set.</param> <param name="Trace">Stage timings for the F12 perf panel, or null.</param>
internal sealed record SearchOutcome(ISearchResult Result, QueryTraceData? Trace);
