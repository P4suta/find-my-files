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
    public void ResultLeaseGate_DisposeClosesAdmission_AndReleasesAfterDrainExactlyOnce()
    {
        var gate = new ResultLeaseGate();

        Assert.True(gate.TryAcquire());
        Assert.False(gate.Dispose());
        Assert.False(gate.TryAcquire());
        Assert.True(gate.Release());
        Assert.False(gate.Dispose());
    }

    [Fact]
    public async Task Connection_RunsFixedHandshake_AndSynthesizesEvents()
    {
        using var server = new FakePipeServer
        {
            Statuses = [new("C:", VolumeState.Ready, 42), new("D:", VolumeState.Scanning, 7)],
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var log = new List<string>();
        var gate = new System.Threading.Lock();
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
                    return log.Contains("index-changed", StringComparer.Ordinal);
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
    public async Task Connection_IsNotPublishedWhileHelloIsPending()
    {
        var hello = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var indexStatus = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode switch
            {
                PipeProtocol.Op.Hello => hello.Task,
                PipeProtocol.Op.IndexStatus => indexStatus.Task,
                _ => null,
            },
        };
        using var client = new PipeEngineClient(server.PipeName);
        var eventsBeforePublication = 0;
        client.VolumeUpdated += _ => Interlocked.Increment(ref eventsBeforePublication);

        await server.WaitForAsync(PipeProtocol.Op.Hello);
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.ListVolumesAsync());
        Assert.DoesNotContain(PipeProtocol.Op.ListVolumes, server.OpcodesOf(0));
        Assert.Equal(EngineConnectionState.Connecting, client.Connection);

        hello.SetResult((
            PipeProtocol.Status.Ok,
            PipeProtocol.EncodeHelloResp(
                PipeProtocol.ProtocolVersion,
                EngineContract.AbiVersion,
                4242)));
        await server.WaitForAsync(PipeProtocol.Op.IndexStatus);
        await server.SendEventAsync((uint)EventKind.VolumeReady, 123, "C:");
        await Task.Delay(50);
        Assert.Equal(0, Volatile.Read(ref eventsBeforePublication));
        Assert.Equal(EngineConnectionState.Connecting, client.Connection);

        indexStatus.SetResult((
            PipeProtocol.Status.Ok,
            PipeProtocol.EncodeVolumeStatuses(server.Statuses)));
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected after Hello");
        Assert.Equal(["C:"], await client.ListVolumesAsync());
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
    public async Task PresentationBasis_FromAnotherPipeClient_BehavesAsNoBasis()
    {
        using var firstServer = new FakePipeServer();
        using var secondServer = new FakePipeServer();
        ulong? observedBasis = null;
        secondServer.Handler = (opcode, payload) =>
        {
            if (opcode == PipeProtocol.Op.Query)
            {
                observedBasis = PipeProtocol.DecodeQueryReq(payload).PresentationBasis;
            }

            return null;
        };
        using var firstClient = new PipeEngineClient(firstServer.PipeName);
        using var secondClient = new PipeEngineClient(secondServer.PipeName);
        await WaitUntilAsync(
            () => firstClient.Connection == EngineConnectionState.Connected
                && secondClient.Connection == EngineConnectionState.Connected,
            "both clients connected");

        var basis = await firstClient.SearchAsync("a", SearchOptions.Default);
        var outcome = await secondClient.SearchAsync(
            "b",
            SearchOptions.Default,
            basis.Result);

        Assert.Equal(0UL, observedBasis);
        outcome.Result.Dispose();
        basis.Result.Dispose();
    }

    [Fact]
    public async Task Dispose_WaitsForPresentationBasisUse_BeforeResultFree()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var basis = await client.SearchAsync("a", SearchOptions.Default);

        var response = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        server.Handler = (opcode, _) =>
            opcode == PipeProtocol.Op.Query ? response.Task : null;
        var next = client.SearchAsync(
            "b",
            SearchOptions.Default,
            basis.Result);
        await WaitUntilAsync(
            () => server.Received.Count(frame => frame.Opcode == PipeProtocol.Op.Query) == 2,
            "basis query admitted");

        basis.Result.Dispose();
        await Task.Delay(100);
        Assert.DoesNotContain(PipeProtocol.Op.ResultFree, server.OpcodesOf(0));

        response.SetResult((
            PipeProtocol.Status.Ok,
            PipeProtocol.EncodeQueryResp(2, 0, "{}")));
        var outcome = await next;
        await server.WaitForAsync(PipeProtocol.Op.ResultFree);
        var free = server.Received.Last(frame => frame.Opcode == PipeProtocol.Op.ResultFree);
        Assert.Equal(1UL, PipeProtocol.DecodeResultFreeReq(free.Payload));
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task Search_MalformedTrace_ReleasesTheDecodedServerResult()
    {
        const ulong resultId = 0xA11C_E001;
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.Query
                ? Task.FromResult((
                    PipeProtocol.Status.Ok,
                    PipeProtocol.EncodeQueryResp(resultId, 1, "{")))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        await Assert.ThrowsAsync<System.Text.Json.JsonException>(
            () => client.SearchAsync("a", SearchOptions.Default));

        await server.WaitForAsync(PipeProtocol.Op.ResultFree);
        var free = server.Received.Last(r => r.Opcode == PipeProtocol.Op.ResultFree);
        Assert.Equal(resultId, PipeProtocol.DecodeResultFreeReq(free.Payload));
    }

    [Fact]
    public async Task Search_CountOutsideManagedRange_ReleasesTheDecodedServerResult()
    {
        const ulong resultId = 0xA11C_E002;
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.Query
                ? Task.FromResult((
                    PipeProtocol.Status.Ok,
                    PipeProtocol.EncodeQueryResp(resultId, ulong.MaxValue, "{}")))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        await Assert.ThrowsAsync<OverflowException>(
            () => client.SearchAsync("a", SearchOptions.Default));

        await server.WaitForAsync(PipeProtocol.Op.ResultFree);
        var free = server.Received.Last(r => r.Opcode == PipeProtocol.Op.ResultFree);
        Assert.Equal(resultId, PipeProtocol.DecodeResultFreeReq(free.Payload));
    }

    [Fact]
    public async Task CancelledQuery_LateResponse_ReleasesTheOrphanedServerResult()
    {
        const ulong resultId = 0xA11C_E003;
        var responseGate = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.Query
                ? responseGate.Task
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        using var cts = new CancellationTokenSource();
        var search = client.SearchAsync("a", SearchOptions.Default, cts.Token);
        await server.WaitForAsync(PipeProtocol.Op.Query);
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => search);

        responseGate.SetResult((
            PipeProtocol.Status.Ok,
            PipeProtocol.EncodeQueryResp(resultId, 1, "{}")));
        await server.WaitForAsync(PipeProtocol.Op.ResultFree);

        var free = server.Received.Last(r => r.Opcode == PipeProtocol.Op.ResultFree);
        Assert.Equal(resultId, PipeProtocol.DecodeResultFreeReq(free.Payload));
    }

    [Fact]
    public async Task ResponseOpcodeMismatch_FailsRequestAndRetiresConnection()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        server.ResponseOpcode = opcode =>
            opcode == PipeProtocol.Op.ListVolumes ? PipeProtocol.Op.Stats : opcode;

        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.ListVolumesAsync());
        await WaitUntilAsync(
            () => server.ConnectionCount >= 2,
            "protocol-violating connection retired");
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
    public async Task RequestTimeout_RetiresHungEpoch_AndReconnects()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName)
        {
            RequestTimeout = TimeSpan.FromMilliseconds(75),
        };
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        var hung = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        server.Handler = (opcode, _) =>
            opcode == PipeProtocol.Op.ListVolumes ? hung.Task : null;

        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.ListVolumesAsync());
        await WaitUntilAsync(
            () => server.ConnectionCount >= 2
                && client.Connection == EngineConnectionState.Connected,
            "hung epoch retired and reconnected");

        server.Handler = null;
        Assert.Equal(["C:"], await client.ListVolumesAsync());
    }

    [Fact]
    public async Task OldEpochTimeout_CannotRetireReplacementConnection()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName)
        {
            RequestTimeout = TimeSpan.FromMilliseconds(75),
        };
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "first connection");

        var hung = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        server.Handler = (opcode, _) =>
            opcode == PipeProtocol.Op.ListVolumes ? hung.Task : null;

        var timeoutReached = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var allowTimeoutRetire = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        client.BeforeTimeoutRetireForTests = () =>
        {
            timeoutReached.TrySetResult();
            return allowTimeoutRetire.Task;
        };

        var oldRequest = client.ListVolumesAsync();
        await timeoutReached.Task.WaitAsync(TimeSpan.FromSeconds(5));

        // Replace the timed-out request's epoch while its catch path is
        // deliberately paused immediately before retirement.
        server.DisconnectAll();
        await WaitUntilAsync(
            () => server.ConnectionCount == 2
                && client.Connection == EngineConnectionState.Connected,
            "replacement connection");

        server.Handler = null;
        client.BeforeTimeoutRetireForTests = null;
        allowTimeoutRetire.SetResult();
        await Assert.ThrowsAsync<EngineUnavailableException>(() => oldRequest);

        Assert.Equal(["C:"], await client.ListVolumesAsync());
        await Task.Delay(750);
        Assert.Equal(2, server.ConnectionCount);
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
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
        Assert.Equal(EngineContract.AbiVersion, stats.Transport.AbiVersion);
    }

    [Fact]
    public async Task ProtocolMismatch_IsFatal_NoReconnect()
    {
        // A version the client does not speak (one past the current) — e.g. a
        // stale or future service binary — must be a fatal, no-reconnect mismatch.
        using var server = new FakePipeServer { ProtocolVersion = PipeProtocol.ProtocolVersion + 1 };
        using var client = new PipeEngineClient(server.PipeName);

        await WaitUntilAsync(() => server.ConnectionCount == 1, "first connection");
        await server.WaitForAsync(PipeProtocol.Op.Hello);

        // The 250ms backoff would have produced a retry well within this
        // window — a fatal mismatch must not.
        await Task.Delay(750);

        Assert.Equal(1, server.ConnectionCount);
        Assert.Equal(new[] { PipeProtocol.Op.Hello }, server.OpcodesOf(0)); // no Subscribe
        Assert.Equal(EngineConnectionState.Faulted, client.Connection);
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.SearchAsync("a", SearchOptions.Default));
    }

    [Fact]
    public async Task AbiMismatch_IsInformational_AndDoesNotBlockProtocolCompatiblePeer()
    {
        using var server = new FakePipeServer
        {
            AbiVersion = EngineContract.AbiVersion + 1,
        };
        using var client = new PipeEngineClient(server.PipeName);

        await WaitUntilAsync(() => server.ConnectionCount == 1, "first connection");
        await server.WaitForAsync(PipeProtocol.Op.Subscribe);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected despite ABI mismatch");

        Assert.Equal(1, server.ConnectionCount);
        Assert.Equal(
            new[]
            {
                PipeProtocol.Op.Hello,
                PipeProtocol.Op.Subscribe,
                PipeProtocol.Op.IndexStatus,
            },
            server.OpcodesOf(0));
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
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
        var errors = new List<EngineErrorSeverity>();
        var indexChanged = new List<string>();
        var gate = new System.Threading.Lock();
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
        await server.SendEventAsync(
            (uint)EventKind.EngineError,
            (ulong)EngineErrorSeverity.Error,
            "C:");
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
            Assert.Equal([EngineErrorSeverity.Error], errors);
        }
    }

    [Fact]
    public async Task EventPush_InvalidEngineErrorSeverity_DropsTheConnection()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var errors = 0;
        client.EngineErrorOccurred += _ => Interlocked.Increment(ref errors);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");

        await server.SendEventAsync(
            (uint)EventKind.EngineError,
            (ulong)EngineErrorSeverity.Panic + 1,
            "C:");

        await WaitUntilAsync(
            () => server.ConnectionCount >= 2
                && client.Connection == EngineConnectionState.Connected,
            "malformed event connection retired and reconnected");
        Assert.Equal(0, Volatile.Read(ref errors));
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
