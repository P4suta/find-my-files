using System.Text.Json;

namespace FindMyFiles.Engine;

/// <summary>
/// The engine boundary's JSON dialect — snake_case keys, matching serde's
/// casing on the Rust side — defined exactly once (ADR-0018; the audit found
/// the same options duplicated per consumer, which is how casing drifts).
/// Every (de)serialization of engine contract JSON (query traces, stats
/// snapshots, pipe JSON payloads, golden fixtures) must use
/// <see cref="SnakeCase"/>. UI-owned files (settings.json) are a different
/// format and deliberately keep their own options.
/// </summary>
internal static class EngineJson
{
    internal static readonly JsonSerializerOptions SnakeCase = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
    };
}
