using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class FakeEngineClientTests
{
    private static readonly string[] FocusedExtensions = [".txt", ".pdf"];

    [Fact]
    public async Task FocusedRewrite_AppliesExtensionAndExcludeConstraints()
    {
        using var engine = new FakeEngineClient();
        var outcome = await engine.SearchAsync(
            @"file_0 !path:""\windows\"" ext:txt;pdf",
            SearchOptions.Default,
            TestContext.Current.CancellationToken);
        using var result = outcome.Result;

        Assert.True(result.Count > 0);
        var page = await result.GetRangeAsync(0, EngineContract.MaxPageRows, TestContext.Current.CancellationToken);
        Assert.NotEmpty(page);
        Assert.All(
            page,
            row => Assert.Contains(
                Path.GetExtension(row.Name),
                FocusedExtensions,
                StringComparer.OrdinalIgnoreCase));
        Assert.All(
            page,
            row => Assert.DoesNotContain(
                @"\windows\",
                row.FullPath,
                StringComparison.OrdinalIgnoreCase));
    }

#if DEBUG
    [Fact]
    public async Task FaultInjection_reproduces_every_token_the_service_implements()
    {
        using var engine = new FakeEngineClient();

        await Assert.ThrowsAsync<EngineException>(
            () => engine.SearchAsync("!!panic", SearchOptions.Default, TestContext.Current.CancellationToken));

        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => engine.SearchAsync("!!drop", SearchOptions.Default, TestContext.Current.CancellationToken));

        var warned = await engine.SearchAsync("!!warn", SearchOptions.Default, TestContext.Current.CancellationToken);
        warned.Result.Dispose();
        var stats = await engine.GetStatsAsync(TestContext.Current.CancellationToken);
        Assert.NotNull(stats);
        Assert.Contains(
            stats.RecentErrors,
            e => string.Equals(e.Severity, "warn", StringComparison.Ordinal)
                && e.Message.StartsWith("fault injection:", StringComparison.Ordinal));

        var lagged = await engine.SearchAsync("!!lag file_0", SearchOptions.Default, TestContext.Current.CancellationToken);
        using var laggedResult = lagged.Result;
        Assert.True(laggedResult.Count > 0);
    }
#endif

    [Fact]
    public async Task HiddenSystemConstraint_StillHonorsTheSearchOption()
    {
        using var engine = new FakeEngineClient();
        var hidden = SearchOptions.Default with { IncludeHiddenSystem = true };

        var excludedOutcome = await engine.SearchAsync("hidden_sys ext:dat", SearchOptions.Default, TestContext.Current.CancellationToken);
        using var excluded = excludedOutcome.Result;
        Assert.Equal(0, excluded.Count);

        var includedOutcome = await engine.SearchAsync("hidden_sys ext:dat", hidden, TestContext.Current.CancellationToken);
        using var included = includedOutcome.Result;
        Assert.True(included.Count > 0);
    }
}
