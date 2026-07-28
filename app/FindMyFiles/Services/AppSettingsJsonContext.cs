using System.Text.Json.Serialization;

namespace FindMyFiles.Services;

/// <summary>
/// Source-generated JSON contract for <see cref="AppSettings"/>: compile-time
/// (de)serialization metadata in place of reflection, pinning the snake_case,
/// indented shape written by the settings UI. Mirrors the codebase's
/// source-generator posture (<c>[LibraryImport]</c>, <c>[ObservableProperty]</c>)
/// and is trim/AOT-forward — the settings type is small and its schema is stable.
/// </summary>
[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower,
    WriteIndented = true,
    UnmappedMemberHandling = JsonUnmappedMemberHandling.Disallow)]
[JsonSerializable(typeof(AppSettings))]
internal sealed partial class AppSettingsJsonContext : JsonSerializerContext
{
}
