using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests;

/// <summary> The connection <em>warm-up</em> path: a freshly registered fmf-service is not serving its pipe yet (writer-lock wait, cold MFT scan, SCM start), so the client's first connect attempts find no pipe.
/// The supervisor must keep retrying (250ms→5s backoff) and reach Connected once the service starts serving — with no external nudge.
/// This is the exact window the shipped onboarding bug fell into: the old code gave up after a fixed ~8s probe and dead-ended on the setup screen.
/// These tests pin that the transport itself rides out an arbitrarily long warm-up, which is what the relaunch-into-pipe fix relies on.
/// </summary>
public sealed class PipeEngineClientWarmupTests
{
    [Fact]
    public async Task ConnectsAfterTheServerStartsServing_NoFixedBudget()
    {
        using var server = new FakePipeServer(startHeld: true);
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);

        var connectedSeen = 0;
        client.ConnectionChanged += s =>
        {
            if (s == EngineConnectionState.Connected)
            {
                Interlocked.Increment(ref connectedSeen);
            }
        };

        client.Start();

        await Task.Delay(600, TestContext.Current.CancellationToken);
        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
        Assert.Equal(0, Volatile.Read(ref connectedSeen));
        Assert.Equal(0, server.ConnectionCount);

        server.ReleaseAccept();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected after warm-up");

        Assert.Equal(1, server.ConnectionCount);
        Assert.Equal(1, Volatile.Read(ref connectedSeen));
    }

    [Fact]
    public async Task WarmUpConnect_RunsTheFullHandshake_AndIsUsable()
    {
        using var server = new FakePipeServer(startHeld: true)
        {
            Rows = Rows.Many(3, "warm"),
        };
        using var client = new PipeEngineClient(server.PipeName);

        await Task.Delay(300, TestContext.Current.CancellationToken);
        Assert.NotEqual(EngineConnectionState.Connected, client.Connection);

        server.ReleaseAccept();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        Assert.Equal(
            new[] { PipeProtocol.Op.Hello, PipeProtocol.Op.Subscribe, PipeProtocol.Op.IndexStatus },
            server.OpcodesOf(0));

        var outcome = await client.SearchAsync("warm", SearchOptions.Default, TestContext.Current.CancellationToken);
        Assert.Equal(3, outcome.Result.Count);
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task RequestDuringWarmUp_FailsFast_NotAfterTheRequestTimeout()
    {
        using var server = new FakePipeServer(startHeld: true);
        using var client = new PipeEngineClient(server.PipeName);

        await Task.Delay(100, TestContext.Current.CancellationToken);
        Assert.NotEqual(EngineConnectionState.Connected, client.Connection);

        var sw = System.Diagnostics.Stopwatch.StartNew();
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.SearchAsync("a", SearchOptions.Default, TestContext.Current.CancellationToken));
        sw.Stop();

        Assert.True(
            sw.Elapsed < TimeSpan.FromSeconds(5),
            $"warm-up request took {sw.Elapsed} — should fail fast");
    }
}
