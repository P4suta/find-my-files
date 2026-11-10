using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests;

/// <summary>Exercises a held server, handshake readiness, and requests before connection.</summary>
public sealed class PipeEngineClientWarmupTests
{
    [Fact]
    public async Task ConnectsAfterTheServerStartsServing_NoFixedBudget()
    {
        using var server = new FakePipeServer(startHeld: true);
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var notificationHeld = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var releaseNotification = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        client.ConnectionChanged += state =>
        {
            if (state == EngineConnectionState.Connected)
            {
                notificationHeld.TrySetResult();
                releaseNotification.Task.GetAwaiter().GetResult();
            }
        };

        var connectedSeen = 0;
        client.ConnectionChanged += s =>
        {
            if (s == EngineConnectionState.Connected)
            {
                Interlocked.Increment(ref connectedSeen);
            }
        };

        client.Start();

        try
        {
            await Task.Delay(600);
            Assert.Equal(EngineConnectionState.Connecting, client.Connection);
            Assert.Equal(0, Volatile.Read(ref connectedSeen));
            Assert.Equal(0, server.ConnectionCount);

            server.ReleaseAccept();
            await notificationHeld.Task.WaitAsync(TimeSpan.FromSeconds(5));
            await WaitUntilAsync(
                () => client.Connection == EngineConnectionState.Connected,
                "connected after warm-up");

            Assert.Equal(0, Volatile.Read(ref connectedSeen));
            releaseNotification.TrySetResult();
            await WaitUntilAsync(
                () => Volatile.Read(ref connectedSeen) >= 1,
                "connected notification after warm-up");

            Assert.Equal(1, server.ConnectionCount);
            Assert.Equal(1, Volatile.Read(ref connectedSeen));
        }
        finally
        {
            releaseNotification.TrySetResult();
        }
    }

    [Fact]
    public async Task WarmUpConnect_RunsTheFullHandshake_AndIsUsable()
    {
        using var server = new FakePipeServer(startHeld: true)
        {
            Rows = Rows.Many(3, "warm"),
        };
        using var client = new PipeEngineClient(server.PipeName);

        await Task.Delay(300);
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
