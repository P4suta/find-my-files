using System.Text.Json.Serialization;

namespace FindMyFiles.Engine;

/// <summary>
/// Source-generated JSON metadata for the engine boundary's snake_case dialect
/// (ADR-0018): compile-time (de)serialization in place of reflection, mirroring
/// <see cref="Services.AppSettingsJsonContext"/> and trim/AOT-forward. Every
/// engine-contract type that crosses the JSON boundary — query traces, stats
/// snapshots, the volume-status and index-start payloads — is registered here.
/// <see cref="EngineJson.SnakeCase"/> exposes this context's options, so all
/// callers route through the one definition and the snake_case casing (and the
/// resolver) never drift.
/// </summary>
[JsonSourceGenerationOptions(PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower)]
[JsonSerializable(typeof(QueryTraceData))]
[JsonSerializable(typeof(EngineStatsData))]
[JsonSerializable(typeof(ServiceInfoData))]
[JsonSerializable(typeof(List<PipeProtocol.VolumeStatusJson>))]
[JsonSerializable(typeof(PipeProtocol.IndexStartJson))]
internal sealed partial class EngineJsonContext : JsonSerializerContext
{
}
