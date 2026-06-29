using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests;

/// <summary>
/// The connection <em>warm-up</em> path: a freshly registered fmf-service is not
/// serving its pipe yet (writer-lock wait, cold MFT scan, SCM start), so the
/// client's first connect attempts find no pipe. The supervisor must keep
/// retrying (250ms→5s backoff) and reach Connected once the service starts
/// serving — with no external nudge. This is the exact window the shipped
/// onboarding bug fell into: the old code gave up after a fixed ~8s probe and
/// dead-ended on the setup screen. These tests pin that the transport itself
/// rides out an arbitrarily long warm-up, which is what the relaunch-into-pipe
/// fix relies on.
/// </summary>
public sealed class PipeEngineClientWarmupTests
{
    [Fact]
    public async Task ConnectsAfterTheServerStartsServing_NoFixedBudget()
    {
        // Server exists but is NOT accepting yet (held): the pipe doesn't exist, so
        // every connect attempt fails — the warm-up window.
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

        // While the server is held, the client cannot connect: it stays Connecting
        // and keeps retrying. (Long enough to outlast the 250ms initial backoff —
        // the old fixed-probe path would already have given up by ~8s.)
        await Task.Delay(600);
        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
        Assert.Equal(0, Volatile.Read(ref connectedSeen));
        Assert.Equal(0, server.ConnectionCount); // no connection ever accepted yet

        // The service starts serving — the supervisor's next retry connects, with
        // no help from the caller.
        server.ReleaseAccept();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected after warm-up");

        Assert.Equal(1, server.ConnectionCount);
        Assert.Equal(1, Volatile.Read(ref connectedSeen)); // Connecting→Connected fired once
    }

    [Fact]
    public async Task WarmUpConnect_RunsTheFullHandshake_AndIsUsable()
    {
        // After riding out the warm-up, the connection is a real one: fixed
        // handshake replayed and requests succeed — not a half-open socket.
        using var server = new FakePipeServer(startHeld: true)
        {
            Rows = Rows.Many(3, "warm"),
        };
        using var client = new PipeEngineClient(server.PipeName);

        await Task.Delay(300); // sit in the warm-up window
        Assert.NotEqual(EngineConnectionState.Connected, client.Connection);

        server.ReleaseAccept();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        Assert.Equal(
            new[] { PipeProtocol.Op.Hello, PipeProtocol.Op.Subscribe, PipeProtocol.Op.IndexStatus },
            server.OpcodesOf(0));

        var outcome = await client.SearchAsync("warm", SearchOptions.Default);
        Assert.Equal(3, outcome.Result.Count);
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task RequestDuringWarmUp_FailsFast_NotAfterTheRequestTimeout()
    {
        // A request issued before the service is up must fail immediately
        // (no connection to write to), never block on the 10s per-request deadline.
        using var server = new FakePipeServer(startHeld: true);
        using var client = new PipeEngineClient(server.PipeName);

        await Task.Delay(100);
        Assert.NotEqual(EngineConnectionState.Connected, client.Connection);

        var sw = System.Diagnostics.Stopwatch.StartNew();
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.SearchAsync("a", SearchOptions.Default));
        sw.Stop();

        Assert.True(
            sw.Elapsed < TimeSpan.FromSeconds(5),
            $"warm-up request took {sw.Elapsed} — should fail fast");
    }
}
