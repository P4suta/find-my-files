using System.Text.Json;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class EngineJsonTests
{
    [Fact]
    public void Exact_version_contract_rejects_unknown_fields()
    {
        const string json = """{"driver":"sweep","unknown_contract_field":1}""";

        Assert.Throws<JsonException>(
            () => JsonSerializer.Deserialize<QueryTraceData>(json, EngineJson.SnakeCase));
    }
}
