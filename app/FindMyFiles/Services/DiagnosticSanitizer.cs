using System.Text.Json;
using System.Text.Json.Nodes;
using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>
/// Privacy boundary for diagnostics copied outside the process. Engine
/// snapshots are useful as numeric evidence, but every string is treated as
/// sensitive because volume labels, errors, or future fields can
/// carry file names and queries. This fail-closed rule also protects newly
/// added contract fields without requiring a second review.
/// </summary>
internal static class DiagnosticSanitizer
{
    internal static string SerializeStats(EngineStatsData stats)
    {
        ArgumentNullException.ThrowIfNull(stats);
        var node = JsonSerializer.SerializeToNode(stats, EngineJson.SnakeCase);
        RedactStrings(node);
        return node?.ToJsonString(IndentedJson) ?? "null";
    }

    private static void RedactStrings(JsonNode? node)
    {
        switch (node)
        {
            case JsonObject obj:
                foreach (var property in obj.ToArray())
                {
                    if (property.Value is JsonValue value
                        && value.TryGetValue<string>(out _))
                    {
                        obj[property.Key] = "[redacted]";
                    }
                    else
                    {
                        RedactStrings(property.Value);
                    }
                }

                break;
            case JsonArray array:
                for (var index = 0; index < array.Count; index++)
                {
                    if (array[index] is JsonValue value
                        && value.TryGetValue<string>(out _))
                    {
                        array[index] = "[redacted]";
                    }
                    else
                    {
                        RedactStrings(array[index]);
                    }
                }

                break;
        }
    }

    private static readonly JsonSerializerOptions IndentedJson = new(EngineJson.SnakeCase)
    {
        WriteIndented = true,
    };
}
