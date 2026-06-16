using System.Text.Json;
using FindMyFiles.Engine;
using Xunit;
using Xunit.Abstractions;

namespace FindMyFiles.Tests.Contract;

/// <summary>
/// The executable contract of the "same mouth" (CLAUDE.md アーキテクチャ固定則):
/// every <see cref="IEngineClient"/> the app can be handed must show the same
/// observable behavior, so these facts run unchanged against all of them.
/// Derived classes only provide the client and optional hooks:
///
///   FakeClientContractTests          — always on (in-proc fake)
///   PipeOverFakeServerContractTests  — always on (wire via FakePipeServer)
///   PipeRealServiceContractTests     — FMF_PIPE_TESTS=1 (`just test-pipe`)
///   FfiInProcContractTests           — FMF_ADMIN_TESTS=1 (loads fmf_engine.dll)
///
/// Gated derivations report and return green when their environment variable
/// is absent (PipeIntegrationTests の流儀), so plain `just test-app` stays
/// hermetic. A derivation that cannot provide the stale hook skips that one
/// fact the same way.
/// </summary>
public abstract class EngineClientContractTests(ITestOutputHelper output) : IAsyncLifetime
{
    private IEngineClient? _client;

    /// <summary>Connected, ready-to-use client — or null when this
    /// derivation's environment gate is closed (every fact then reports and
    /// returns green).</summary>
    protected abstract Task<IEngineClient?> AcquireClientOrSkipAsync();

    /// <summary>Teardown for whatever <see cref="AcquireClientOrSkipAsync"/>
    /// stood up (server process, temp dirs); the client itself is disposed
    /// by the base class before this runs.</summary>
    protected virtual Task TeardownAsync() => Task.CompletedTask;

    /// <summary>A query this implementation accepts. Hit count may be 0
    /// (real service / FFI run without volumes — partial conformance).</summary>
    protected abstract string ValidQuery { get; }

    /// <summary>True for clients whose Connection is the fixed InProc state;
    /// pipe derivations return false and must be Connected after acquire.</summary>
    protected virtual bool IsInProcTransport => true;

    /// <summary>Make every live result of <paramref name="client"/> stale
    /// (index rebuilt / connection epoch turned) and leave the client able
    /// to serve a fresh search. Returns false when this implementation has
    /// no way to force staleness — the stale fact skips.</summary>
    protected virtual Task<bool> TryForceStaleAsync(IEngineClient client) =>
        Task.FromResult(false);

    /// <summary>The shared syntax fixture (contract/golden) — the same file
    /// the Rust parser tests and FakeEngineClient pin.</summary>
    protected static IReadOnlyList<string> GoldenInvalidQueries()
    {
        var dir = Environment.GetEnvironmentVariable("FMF_GOLDEN_DIR")
            ?? Path.Combine(AppContext.BaseDirectory, "golden");
        using var doc = JsonDocument.Parse(
            File.ReadAllBytes(Path.Combine(dir, "invalid_queries.json")));
        return [.. doc.RootElement.GetProperty("queries").EnumerateArray()
            .Select(q => q.GetString()!)];
    }

    public async Task InitializeAsync() => _client = await AcquireClientOrSkipAsync();

    public async Task DisposeAsync()
    {
        _client?.Dispose();
        await TeardownAsync();
    }

    /// <summary>Null means the gate is closed — report once, pass green.</summary>
    private IEngineClient? ClientOrReport()
    {
        if (_client is null)
        {
            output.WriteLine("environment gate closed — contract fact skipped");
        }

        return _client;
    }

    [Fact]
    public async Task InvalidQueries_FromTheSharedFixture_ThrowQuerySyntaxException()
    {
        if (ClientOrReport() is not { } client)
        {
            return;
        }

        foreach (var query in GoldenInvalidQueries())
        {
            await Assert.ThrowsAsync<QuerySyntaxException>(
                () => client.SearchAsync(query, SearchOptions.Default));
        }

        // A syntax error must not poison the client: a valid query still works.
        var outcome = await client.SearchAsync(ValidQuery, SearchOptions.Default);
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task ValidQuery_CountIsHonest_AndGetRangeRespectsTheBounds()
    {
        if (ClientOrReport() is not { } client)
        {
            return;
        }

        var outcome = await client.SearchAsync(ValidQuery, SearchOptions.Default);
        using var result = outcome.Result;
        Assert.True(result.Count >= 0, "Count must never be negative");

        // A page inside the range is filled exactly up to min(want, Count).
        var page = await result.GetRangeAsync(0, 5);
        Assert.Equal((int)Math.Min(5, result.Count), page.Count);

        // A page at/after the end is empty — never an exception, never junk.
        var beyond = await result.GetRangeAsync(result.Count, 5);
        Assert.Empty(beyond);
    }

    [Fact]
    public async Task SearchAsync_PreCancelledToken_ThrowsOperationCanceled()
    {
        if (ClientOrReport() is not { } client)
        {
            return;
        }

        using var cts = new CancellationTokenSource();
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(
            () => client.SearchAsync(ValidQuery, SearchOptions.Default, cts.Token));
    }

    [Fact]
    public async Task GetRangeAsync_PreCancelledToken_ThrowsOperationCanceled()
    {
        if (ClientOrReport() is not { } client)
        {
            return;
        }

        var outcome = await client.SearchAsync(ValidQuery, SearchOptions.Default);
        using var result = outcome.Result;
        using var cts = new CancellationTokenSource();
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(
            () => result.GetRangeAsync(0, 1, cts.Token));
    }

    [Fact]
    public async Task StaleResult_ThrowsStaleResultException_AndAFreshSearchRecovers()
    {
        if (ClientOrReport() is not { } client)
        {
            return;
        }

        var outcome = await client.SearchAsync(ValidQuery, SearchOptions.Default);
        using var result = outcome.Result;
        if (!await TryForceStaleAsync(client))
        {
            output.WriteLine("no stale hook for this implementation — fact skipped");
            return;
        }

        await Assert.ThrowsAsync<StaleResultException>(() => result.GetRangeAsync(0, 1));

        // Stale is recoverable by re-running the query, never terminal.
        var again = await client.SearchAsync(ValidQuery, SearchOptions.Default);
        again.Result.Dispose();
    }

    [Fact]
    public async Task Connection_ReportsTheDeclaredTransportState()
    {
        if (ClientOrReport() is not { } client)
        {
            return;
        }

        if (IsInProcTransport)
        {
            Assert.Equal(EngineConnectionState.InProc, client.Connection);

            // …and it stays InProc across use (fixed, no transitions).
            var outcome = await client.SearchAsync(ValidQuery, SearchOptions.Default);
            outcome.Result.Dispose();
            Assert.Equal(EngineConnectionState.InProc, client.Connection);
        }
        else
        {
            // Pipe transports were acquired connected and are never InProc.
            Assert.Equal(EngineConnectionState.Connected, client.Connection);
        }
    }
}
