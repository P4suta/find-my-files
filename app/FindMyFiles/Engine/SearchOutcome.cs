namespace FindMyFiles.Engine;

// Data shapes of the engine boundary (the DTO half of IEngineClient.cs;
// ADR-0018). JSON-backed types deserialize with EngineJson.SnakeCase and
// mirror the golden fixtures in contract/golden/ — GoldenCorpusTests pins
// every field against the same files the Rust suite pins.

/// <summary>What <see cref="IEngineClient.SearchAsync"/> returns: the
/// materialized <see cref="ISearchResult"/> the UI pages through, paired with
/// the per-query <see cref="QueryTraceData"/> the engine attached (null when
/// tracing was unavailable, e.g. a serialization failure — the result is
/// still valid).</summary>
/// <param name="Result">The sort-ordered, O(1)-paged result set.</param>
/// <param name="Trace">Stage timings for the F12 perf panel, or null.</param>
public sealed record SearchOutcome(ISearchResult Result, QueryTraceData? Trace);
