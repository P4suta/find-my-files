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
    /// <summary>
    /// The engine-JSON options, backed by the source-generated
    /// <see cref="EngineJsonContext"/> (compile-time metadata in place of
    /// reflection, trim/AOT-forward; mirrors the codebase's source-gen posture).
    /// Returns the context's cached singleton — callers needing a tweak (the
    /// indented diag dump) derive from it, so the casing and resolver stay
    /// defined in one place. Every registered engine type resolves through it.
    /// </summary>
    internal static JsonSerializerOptions SnakeCase => EngineJsonContext.Default.Options;
}
