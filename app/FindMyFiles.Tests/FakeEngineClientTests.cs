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
            SearchOptions.Default);
        using var result = outcome.Result;

        Assert.True(result.Count > 0);
        var page = await result.GetRangeAsync(0, EngineContract.MaxPageRows);
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
        // The fake is the engine a developer can reach without installing a
        // service, so a fault token it does not implement is a fault that can
        // only be reproduced by the people who least need to. `!!drop` was
        // that token: a response that never arrives is the shape of failure
        // that looks like a hang, and it was reachable only through the pipe.
        // (xtask's fault_injection_tokens_match_on_both_engines holds the two
        // token sets equal; this pins what the fake's half actually does.)
        using var engine = new FakeEngineClient();

        await Assert.ThrowsAsync<EngineException>(
            () => engine.SearchAsync("!!panic", SearchOptions.Default));

        // The service severs the pipe; the fake has none, so it raises what
        // the pipe client raises once its connection is gone.
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => engine.SearchAsync("!!drop", SearchOptions.Default));

        // `!!warn` and `!!lag` degrade rather than fail: the query still runs.
        var warned = await engine.SearchAsync("!!warn", SearchOptions.Default);
        warned.Result.Dispose();
        var stats = await engine.GetStatsAsync();
        Assert.NotNull(stats);
        Assert.Contains(
            stats.RecentErrors,
            e => string.Equals(e.Severity, "warn", StringComparison.Ordinal)
                && e.Message.StartsWith("fault injection:", StringComparison.Ordinal));

        var lagged = await engine.SearchAsync("!!lag file_0", SearchOptions.Default);
        using var laggedResult = lagged.Result;
        Assert.True(laggedResult.Count > 0);
    }
#endif

    [Fact]
    public async Task HiddenSystemConstraint_StillHonorsTheSearchOption()
    {
        using var engine = new FakeEngineClient();
        var hidden = SearchOptions.Default with { IncludeHiddenSystem = true };

        var excludedOutcome = await engine.SearchAsync("hidden_sys ext:dat", SearchOptions.Default);
        using var excluded = excludedOutcome.Result;
        Assert.Equal(0, excluded.Count);

        var includedOutcome = await engine.SearchAsync("hidden_sys ext:dat", hidden);
        using var included = includedOutcome.Result;
        Assert.True(included.Count > 0);
    }
}
