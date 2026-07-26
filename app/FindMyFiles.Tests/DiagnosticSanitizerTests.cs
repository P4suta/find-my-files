using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class DiagnosticSanitizerTests
{
    [Fact]
    public void SerializeStats_redacts_every_string_but_keeps_numeric_evidence()
    {
        const string secret = "C:\\Users\\alice\\secret-query.txt";
        var stats = new EngineStatsData
        {
            P99Us = 1234,
            RecentQueries = [new QueryTraceData { Driver = secret, QueryLength = 7 }],
            Indexes = [new IndexStatsData { Volume = secret, Entries = 42 }],
            RecentErrors = [new ErrorEventData { Message = secret, Area = secret }],
        };

        var json = DiagnosticSanitizer.SerializeStats(stats);

        Assert.DoesNotContain(secret, json, StringComparison.Ordinal);
        Assert.Contains("\"p99_us\": 1234", json, StringComparison.Ordinal);
        Assert.Contains("\"query_length\": 7", json, StringComparison.Ordinal);
        Assert.Contains("\"entries\": 42", json, StringComparison.Ordinal);
        Assert.Contains("[redacted]", json, StringComparison.Ordinal);
    }
}
