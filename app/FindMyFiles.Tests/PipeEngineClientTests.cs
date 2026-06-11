using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests;

/// <summary>
/// PipeEngineClient against an in-test <see cref="FakePipeServer"/> on a
/// unique pipe name: handshake order, frame reassembly, disconnect
/// fail-fast, reconnection, fatal version mismatch and dispose ordering.
/// </summary>
public sealed class PipeEngineClientTests
{
    [Fact]
    public async Task Connection_RunsFixedHandshake_AndSynthesizesEvents()
    {
        using var server = new FakePipeServer
        {
            Statuses = [new("C:", VolumeState.Ready, 42), new("D:", VolumeState.Scanning, 7)],
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var log = new List<string>();
        var gate = new object();
        client.VolumeUpdated += s =>
        {
            lock (gate)
            {
                log.Add($"volume {s.Label} {s.State} {s.Entries}");
            }
        };
        client.IndexChanged += _ =>
        {
            lock (gate)
            {
                log.Add("index-changed");
            }
        };

        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");
        await WaitUntilAsync(
            () =>
            {
                lock (gate)
                {
                    return log.Contains("index-changed");
                }
            },
            "synthesized IndexChanged");

        // The (re)connect sequence is fixed: Hello → Subscribe → IndexStatus.
        Assert.Equal(
            new[]
            {
                PipeProtocol.Op.Hello, PipeProtocol.Op.Subscribe, PipeProtocol.Op.IndexStatus,
            },
            server.OpcodesOf(0));
        // …and the catch-up events are synthesized locally from IndexStatus:
        // every volume first, then exactly one IndexChanged.
        lock (gate)
        {
            Assert.Equal(
                ["volume C: Ready 42", "volume D: Scanning 7", "index-changed"],
                log);
        }
    }

    [Fact]
    public async Task SearchThenPage_RoundTrips_AcrossChunkedWrites()
    {
        using var server = new FakePipeServer
        {
            ChunkedWrites = true, // 1-byte writes: reassembly must not care
            Rows = Rows.Many(5, "pipe"),
        };
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        var outcome = await client.SearchAsync("pipe", SearchOptions.Default);
        Assert.Equal(5, outcome.Result.Count);

        var rows = await outcome.Result.GetRangeAsync(0, 5);
        Assert.Equal(Rows.Many(5, "pipe"), rows); // record equality, all fields
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task Disconnect_FailsPendingFast_AndStalesLiveResults()
    {
        using var server = new FakePipeServer { Rows = Rows.Many(3) };
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");
        var outcome = await client.SearchAsync("a", SearchOptions.Default);

        // Hold the page response open, then cut the connection under it.
        var gate = new TaskCompletionSource<(int Status, byte[] Payload)>();
        server.Handler = (op, _) => op == PipeProtocol.Op.ResultPage ? gate.Task : null;
        var fetch = outcome.Result.GetRangeAsync(0, 3);
        await server.WaitForAsync(PipeProtocol.Op.ResultPage);
        server.DisconnectAll();

        // Pending requests fail fast — no 10s timeout wait.
        await Assert.ThrowsAsync<EngineUnavailableException>(() => fetch);
        // The surviving handle is epoch-invalidated: stale, not hanging.
        await Assert.ThrowsAsync<StaleResultException>(() => outcome.Result.GetRangeAsync(0, 1));
    }

    [Fact]
    public async Task RequestWhileDisconnected_FailsFast_NotAfterTheRequestTimeout()
    {
        var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        // Server fully gone: accept loop stopped and live connections cut —
        // the supervisor stays in Reconnecting with no connection object.
        server.Dispose();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Reconnecting, "noticed the drop");

        var sw = System.Diagnostics.Stopwatch.StartNew();
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.SearchAsync("a", SearchOptions.Default));
        sw.Stop();
        // There is no connection to write to, so the failure is immediate —
        // never the 10s per-request deadline.
        Assert.True(
            sw.Elapsed < TimeSpan.FromSeconds(5),
            $"disconnected request took {sw.Elapsed} — should fail fast");
    }

    [Fact]
    public async Task Reconnect_RedoesHandshake_AndRefiresIndexChanged()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var indexChanged = 0;
        var sawReconnecting = 0;
        client.IndexChanged += _ => Interlocked.Increment(ref indexChanged);
        client.ConnectionChanged += s =>
        {
            if (s == EngineConnectionState.Reconnecting)
            {
                Interlocked.Increment(ref sawReconnecting);
            }
        };
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "first connect");
        await WaitUntilAsync(
            () => Volatile.Read(ref indexChanged) == 1, "first synthesized IndexChanged");

        server.DisconnectAll();
        await WaitUntilAsync(
            () => server.ConnectionCount == 2
                && client.Connection == EngineConnectionState.Connected,
            "reconnect");
        await WaitUntilAsync(
            () => Volatile.Read(ref indexChanged) == 2, "re-fired IndexChanged");

        Assert.True(Volatile.Read(ref sawReconnecting) >= 1);
        // The second connection replays the full fixed sequence.
        Assert.Equal(
            new[]
            {
                PipeProtocol.Op.Hello, PipeProtocol.Op.Subscribe, PipeProtocol.Op.IndexStatus,
            },
            server.OpcodesOf(1));

        var stats = await client.GetStatsAsync();
        Assert.NotNull(stats);
        Assert.NotNull(stats!.Transport);
        Assert.Equal("Connected", stats.Transport!.State);
        Assert.Equal(1, stats.Transport.Reconnects);
        Assert.Equal(4242u, stats.Transport.ServerPid);
        Assert.Equal(1u, stats.Transport.AbiVersion);
    }

    [Fact]
    public async Task ProtocolMismatch_IsFatal_NoReconnect()
    {
        using var server = new FakePipeServer { ProtocolVersion = 2 };
        using var client = new PipeEngineClient(server.PipeName);

        await WaitUntilAsync(() => server.ConnectionCount == 1, "first connection");
        await server.WaitForAsync(PipeProtocol.Op.Hello);
        // The 250ms backoff would have produced a retry well within this
        // window — a fatal mismatch must not.
        await Task.Delay(750);

        Assert.Equal(1, server.ConnectionCount);
        Assert.Equal(new[] { PipeProtocol.Op.Hello }, server.OpcodesOf(0)); // no Subscribe
        Assert.NotEqual(EngineConnectionState.Connected, client.Connection);
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.SearchAsync("a", SearchOptions.Default));
    }

    [Fact]
    public async Task Dispose_DrainsInFlight_BeforeResultFree()
    {
        using var server = new FakePipeServer { Rows = Rows.Many(4) };
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");
        var outcome = await client.SearchAsync("a", SearchOptions.Default);

        var release = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        async Task<(int Status, byte[] Payload)> HoldAsync()
        {
            await release.Task;
            return (PipeProtocol.Status.Ok, PipeProtocol.EncodePageResp(Rows.Many(4)));
        }
        server.Handler = (op, _) => op == PipeProtocol.Op.ResultPage ? HoldAsync() : null;

        var fetch = outcome.Result.GetRangeAsync(0, 4);
        await server.WaitForAsync(PipeProtocol.Op.ResultPage);

        outcome.Result.Dispose();
        await Task.Delay(100); // a premature ResultFree would land here
        Assert.DoesNotContain(PipeProtocol.Op.ResultFree, server.OpcodesOf(0));

        release.SetResult();
        var rows = await fetch; // the in-flight fetch still completes…
        Assert.Equal(4, rows.Count);
        await server.WaitForAsync(PipeProtocol.Op.ResultFree); // …then the free goes out
    }

    [Fact]
    public async Task EventPush_MapsKindsToTheThreeEvents()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var volumes = new List<VolumeStatus>();
        var errors = new List<int>();
        var indexChanged = new List<string>();
        var gate = new object();
        client.VolumeUpdated += s =>
        {
            lock (gate)
            {
                volumes.Add(s);
            }
        };
        client.EngineErrorOccurred += s =>
        {
            lock (gate)
            {
                errors.Add(s);
            }
        };
        client.IndexChanged += v =>
        {
            lock (gate)
            {
                indexChanged.Add(v);
            }
        };
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        await server.SendEventAsync(2, 123, "C:"); // VolumeReady
        await server.SendEventAsync(3, 0, "C:"); // IndexChanged
        await server.SendEventAsync(6, 2, "C:"); // EngineError severity=2
        await WaitUntilAsync(
            () =>
            {
                lock (gate)
                {
                    return errors.Count == 1;
                }
            },
            "all three events");

        lock (gate)
        {
            Assert.Contains(new VolumeStatus("C:", VolumeState.Ready, 123), volumes);
            Assert.Contains("C:", indexChanged); // the pushed one (plus "*" synthesized)
            Assert.Equal([2], errors);
        }
    }

    [Fact]
    public async Task Probe_AcceptsFullPipePath_AndFailsFastWithoutAServer()
    {
        using var server = new FakePipeServer();

        Assert.True(await PipeEngineClient.ProbeAsync(
            @"\\.\pipe\" + server.PipeName, TimeSpan.FromSeconds(2)));
        Assert.False(await PipeEngineClient.ProbeAsync(
            "fmf-test-no-such-pipe", TimeSpan.FromMilliseconds(250)));
    }
}
