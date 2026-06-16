using FindMyFiles.Engine;

namespace FindMyFiles.ViewModels;

/// <summary>Snapshot of what to search — the ViewModel stays the single
/// source of truth for the UI state; the orchestrator only pulls it.</summary>
/// <param name="Query">Raw user query text (before any focused-mode rewrite).</param>
/// <param name="Options">Sort, case and hidden/system flags for this search.</param>
public readonly record struct SearchRequest(string Query, SearchOptions Options);
