using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Failure/status/event branches of <see cref="PipeEngineClient"/>
/// that are not part of the happy-path state-machine specification.</summary>
public sealed class PipeEngineClientAdditionalTests
{
    private sealed class ThrowingDisposable : IDisposable
    {
        public int Calls { get; private set; }

        public void Dispose()
        {
            Calls++;
            throw new IOException("already broken");
        }
    }

    [Fact]
    public void Synchronous_probe_reports_success_and_failure_without_throwing()
    {
        using var server = new FakePipeServer();

        Assert.True(PipeEngineClient.Probe(server.PipeName, TimeSpan.FromSeconds(2)));
        Assert.False(PipeEngineClient.Probe(
            "fmf-test-no-such-sync-pipe",
            TimeSpan.FromMilliseconds(100)));
    }

    [Fact]
    public async Task Probe_identity_policy_is_handle_bound_and_fail_closed()
    {
        using var server = new FakePipeServer();
        var checks = 0;

        Assert.False(await PipeEngineClient.ProbeAsync(
            server.PipeName,
            TimeSpan.FromSeconds(2),
            verifyServerIdentity: true,
            _ =>
            {
                checks++;
                return false;
            }));
        Assert.Equal(1, checks);
    }

    [Fact]
    public async Task Probe_rejects_a_noncanonical_hello_response_header()
    {
        using var server = new FakePipeServer
        {
            ResponseOpcode = opcode =>
                opcode == PipeProtocol.Op.Hello ? PipeProtocol.Op.Stats : opcode,
        };

        Assert.False(await PipeEngineClient.ProbeAsync(
            server.PipeName,
            TimeSpan.FromSeconds(2),
            verifyServerIdentity: false,
            _ => throw new InvalidOperationException()));
    }

    [Fact]
    public async Task Supervisor_identity_rejection_is_terminal_and_trusted_identity_connects()
    {
        // FakePipeServer owns fire-and-forget accept/response loops. Keep those
        // transport tasks outside xUnit's async tracker so a mutation session
        // cannot attribute an unrelated late completion to this test.
        SyncContext.RunContinuationsInline();
        using var log = new LogCapture();
        using var rejectedServer = new FakePipeServer();
        using (var rejected = new PipeEngineClient(
            rejectedServer.PipeName,
            autoStart: false,
            verifyServerIdentity: true,
            _ => false))
        {
            rejected.Start();
            await WaitUntilAsync(
                () => rejected.Connection == EngineConnectionState.Faulted,
                "identity rejection");
            var failure = await Assert.ThrowsAsync<EngineUnavailableException>(
                () => rejected.ListVolumesAsync());
            Assert.Contains(
                $@"server on \\.\pipe\{rejectedServer.PipeName}",
                failure.Message,
                StringComparison.Ordinal);
            Assert.Contains(
                "refusing to connect (possible pipe squatting; SECURITY.md Threat 4)",
                failure.Message,
                StringComparison.Ordinal);
        }

        Assert.Contains("area=pipe", log.Text, StringComparison.Ordinal);
        Assert.Contains("fatal pipe failure — not reconnecting", log.Text, StringComparison.Ordinal);
        Assert.Contains(
            "error_type=FindMyFiles.Engine.PipeEngineClient+ServerIdentityException",
            log.Text,
            StringComparison.Ordinal);
        await WaitUntilAsync(
            () => rejectedServer.ClosedConnectionCount >= 1,
            "rejected stream disposal");

        using var trustedServer = new FakePipeServer();
        using var trusted = new PipeEngineClient(
            trustedServer.PipeName,
            autoStart: false,
            verifyServerIdentity: true,
            _ => true);
        trusted.Start();
        await WaitUntilAsync(
            () => trusted.Connection == EngineConnectionState.Connected,
            "trusted identity");
    }

    [Fact]
    public void Connection_publication_invariant_and_broken_disposal_are_fail_closed()
    {
        PipeEngineClient.ThrowIfConnectionAlreadyPublished(alreadyPublished: false);
        var error = Assert.Throws<InvalidOperationException>(() =>
            PipeEngineClient.ThrowIfConnectionAlreadyPublished(alreadyPublished: true));
        Assert.Equal("a pipe connection was already published", error.Message);

        PipeEngineClient.SafeDispose(null);
        var broken = new ThrowingDisposable();
        PipeEngineClient.SafeDispose(broken);
        Assert.Equal(1, broken.Calls);
    }

    [Theory]
    [InlineData(PipeProtocol.Status.Ok, false, null)]
    [InlineData(PipeProtocol.Status.Io, false, "Subscribe failed (6): detail")]
    [InlineData(PipeProtocol.Status.Io, true, "Hello failed (6): detail")]
    [InlineData(
        PipeProtocol.Status.InvalidArg,
        true,
        "server rejected protocol version {protocol}: detail")]
    public void Handshake_status_validation_preserves_terminal_and_retryable_failures(
        int status,
        bool protocolHello,
        string? expectedMessage)
    {
        var failure = Record.Exception(() => PipeEngineClient.ValidateHandshakeStatus(
            status,
            "detail"u8.ToArray(),
            protocolHello ? "Hello" : "Subscribe",
            protocolHello));

        if (expectedMessage is null)
        {
            Assert.Null(failure);
        }
        else
        {
            Assert.Equal(
                expectedMessage.Replace(
                    "{protocol}",
                    PipeProtocol.ProtocolVersion.ToString(
                        System.Globalization.CultureInfo.InvariantCulture),
                    StringComparison.Ordinal),
                Assert.IsAssignableFrom<Exception>(failure).Message);
        }
    }

    [Theory]
    [InlineData(0, 100, 100)]
    [InlineData(100, 200, 120)]
    public void Page_rtt_ewma_initializes_then_applies_the_exact_weights(
        double previous,
        double sample,
        double expected) =>
        Assert.Equal(expected, PipeEngineClient.UpdatePageRttEwma(previous, sample));

    [Theory]
    [InlineData(250, 500)]
    [InlineData(500, 1000)]
    [InlineData(5000, 5000)]
    public void Reconnect_backoff_doubles_and_caps_at_five_seconds(
        int currentMilliseconds,
        int expectedMilliseconds) =>
        Assert.Equal(
            TimeSpan.FromMilliseconds(expectedMilliseconds),
            PipeEngineClient.NextBackoff(TimeSpan.FromMilliseconds(currentMilliseconds)));

    [Fact]
    public async Task Direct_public_operations_round_trip_and_dispose_is_idempotent()
    {
        using var server = new FakePipeServer
        {
            Statuses = [new("C:", VolumeState.Ready, 42)],
        };
        var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        Assert.Equal(["C:"], await client.ListVolumesAsync());
        await client.StartIndexingAsync(["C:"]);
        Assert.Equal([new VolumeStatus("C:", VolumeState.Ready, 42)], await client.GetStatusAsync());
        Assert.NotNull(await client.GetStatsAsync());

        client.Dispose();
        client.Dispose();
    }

    [Fact]
    public async Task Start_is_idempotent_and_never_spawns_a_second_supervisor()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);

        client.Start();
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        await Task.Delay(100);

        Assert.Equal(1, server.ConnectionCount);
    }

    [Theory]
    [InlineData(PipeProtocol.Status.QuerySyntax, 0)]
    [InlineData(PipeProtocol.Status.Stale, 1)]
    [InlineData(PipeProtocol.Status.Cancelled, 2)]
    [InlineData(PipeProtocol.Status.Io, 3)]
    public async Task Request_statuses_map_to_the_exact_managed_exception(
        int status,
        int expectedKind)
    {
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ListVolumes
                ? Task.FromResult((status, "detail"u8.ToArray()))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        var exception = await Record.ExceptionAsync(
            async () => await client.ListVolumesAsync());

        Assert.NotNull(exception);
        var actualKind = exception switch
        {
            QuerySyntaxException => 0,
            StaleResultException => 1,
            OperationCanceledException => 2,
            EngineException { Code: PipeProtocol.Status.Io } => 3,
            _ => -1,
        };
        Assert.Equal(expectedKind, actualKind);
        switch (exception)
        {
            case QuerySyntaxException syntax:
                Assert.Equal("detail", syntax.Message);
                break;
            case OperationCanceledException cancelled:
                Assert.Equal("query cancelled by the engine", cancelled.Message);
                break;
            case EngineException engine:
                Assert.Equal($"ListVolumes failed ({status}): detail", engine.Message);
                break;
        }
    }

    [Theory]
    [InlineData(0, "IndexStart")]
    [InlineData(1, "IndexStatus")]
    [InlineData(2, "Query")]
    public async Task Public_operation_failures_name_the_exact_wire_operation(
        int operation,
        string expectedName)
    {
        var opcode = operation switch
        {
            0 => PipeProtocol.Op.IndexStart,
            1 => PipeProtocol.Op.IndexStatus,
            _ => PipeProtocol.Op.Query,
        };
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        server.Handler = (candidate, _) => candidate == opcode
            ? Task.FromResult((PipeProtocol.Status.Io, "detail"u8.ToArray()))
            : null;

        Task InvokeAsync() => operation switch
        {
            0 => client.StartIndexingAsync(["C:"]),
            1 => client.GetStatusAsync(),
            _ => client.SearchAsync("query", SearchOptions.Default),
        };

        var failure = await Assert.ThrowsAsync<EngineException>(InvokeAsync);
        Assert.Equal(
            $"{expectedName} failed ({PipeProtocol.Status.Io}): detail",
            failure.Message);
    }

    [Fact]
    public async Task Page_failure_names_ResultPage_and_preserves_the_result_for_disposal()
    {
        using var server = new FakePipeServer { Rows = [Rows.File(1, "one.txt")] };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var outcome = await client.SearchAsync("one", SearchOptions.Default);
        server.Handler = (opcode, _) => opcode == PipeProtocol.Op.ResultPage
            ? Task.FromResult((PipeProtocol.Status.Io, "detail"u8.ToArray()))
            : null;

        var failure = await Assert.ThrowsAsync<EngineException>(
            () => outcome.Result.GetRangeAsync(0, 1));

        Assert.Equal(
            $"ResultPage failed ({PipeProtocol.Status.Io}): detail",
            failure.Message);
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task A_late_success_for_a_nonquery_request_never_releases_a_fake_result()
    {
        var response = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ListVolumes
                ? response.Task
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        using var cancellation = new CancellationTokenSource();
        var request = client.ListVolumesAsync(cancellation.Token);
        await server.WaitForAsync(PipeProtocol.Op.ListVolumes);

        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => request);
        Assert.DoesNotContain(PipeProtocol.Op.QueryCancel, server.OpcodesOf(0));
        response.SetResult((
            PipeProtocol.Status.Ok,
            PipeProtocol.EncodeQueryResp(55, 0, string.Empty)));
        await Task.Delay(100);

        Assert.DoesNotContain(PipeProtocol.Op.ResultFree, server.OpcodesOf(0));
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
    }

    [Fact]
    public async Task Stats_are_best_effort_when_the_server_rejects_them_or_is_disconnected()
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.Stats
                ? Task.FromResult((PipeProtocol.Status.Io, Array.Empty<byte>()))
                : null,
        };
        using var connected = new PipeEngineClient(server.PipeName, autoStart: false);
        connected.Start();
        await WaitUntilAsync(
            () => connected.Connection == EngineConnectionState.Connected,
            "connected");
        Assert.Null(await connected.GetStatsAsync());

        using var disconnected = new PipeEngineClient("fmf-test-not-started", autoStart: false);
        Assert.Null(await disconnected.GetStatsAsync());
        Assert.Contains("area=pipe", log.Text, StringComparison.Ordinal);
        Assert.Contains("stats unavailable", log.Text, StringComparison.Ordinal);
        Assert.Contains(
            "error_type=FindMyFiles.Engine.EngineUnavailableException",
            log.Text,
            StringComparison.Ordinal);
    }

    [Theory]
    [InlineData(PipeProtocol.Op.Hello, PipeProtocol.Status.InvalidArg, true)]
    [InlineData(PipeProtocol.Op.Hello, PipeProtocol.Status.Io, false)]
    [InlineData(PipeProtocol.Op.Subscribe, PipeProtocol.Status.Io, false)]
    [InlineData(PipeProtocol.Op.IndexStatus, PipeProtocol.Status.Io, false)]
    public async Task Handshake_failures_are_terminal_only_for_protocol_rejection(
        ushort failingOpcode,
        int status,
        bool terminal)
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == failingOpcode
                ? Task.FromResult((status, "rejected"u8.ToArray()))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();

        if (terminal)
        {
            await WaitUntilAsync(
                () => client.Connection == EngineConnectionState.Faulted,
                "terminal protocol failure");
            Assert.Equal(1, server.ConnectionCount);
        }
        else
        {
            await WaitUntilAsync(
                () => server.ConnectionCount >= 2,
                "retry after recoverable handshake failure");
            Assert.NotEqual(EngineConnectionState.Faulted, client.Connection);
        }

        var expectedMessage = terminal
            ? "fatal pipe failure — not reconnecting"
            : "connection attempt failed";
        Assert.Contains(
            log.Text.Split('\n'),
            line => line.Contains("area=pipe", StringComparison.Ordinal)
                && line.Contains(expectedMessage, StringComparison.Ordinal));
    }

    [Fact]
    public async Task Recoverable_handshake_failures_observe_the_initial_retry_backoff()
    {
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.Subscribe
                ? Task.FromResult((PipeProtocol.Status.Io, "retry"u8.ToArray()))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(() => server.ConnectionCount == 1, "first attempt");

        await Task.Delay(100);

        Assert.Equal(1, server.ConnectionCount);
        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
    }

    [Fact]
    public async Task Failed_IndexStatus_with_a_decodable_payload_is_never_published()
    {
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.IndexStatus
                ? Task.FromResult((
                    PipeProtocol.Status.Io,
                    PipeProtocol.EncodeVolumeStatuses([new("C:", VolumeState.Ready, 1)])))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await server.WaitForAsync(PipeProtocol.Op.IndexStatus);

        await Task.Delay(100);

        Assert.NotEqual(EngineConnectionState.Connected, client.Connection);
    }

    [Fact]
    public async Task Every_volume_event_kind_maps_and_a_faulting_subscriber_is_contained()
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var volumes = new List<VolumeStatus>();
        client.VolumeUpdated += volumes.Add;
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        volumes.Clear();

        await server.SendEventAsync((uint)EventKind.Progress, 12, "C:");
        await server.SendEventAsync((uint)EventKind.RescanStarted, 0, "D:");
        await server.SendEventAsync((uint)EventKind.VolumeFailed, 0, "E:");
        await WaitUntilAsync(() => volumes.Count == 3, "three volume events");
        Assert.Equal(
            [
                new VolumeStatus("C:", VolumeState.Scanning, 12),
                new VolumeStatus("D:", VolumeState.Rescanning, 0),
                new VolumeStatus("E:", VolumeState.Failed, 0),
            ],
            volumes);

        client.VolumeUpdated += _ => throw new InvalidOperationException("consumer failed");
        await server.SendEventAsync((uint)EventKind.Progress, 13, "C:");
        await server.SendEventAsync((uint)EventKind.VolumeReady, 13, "C:");
        await server.SendEventAsync((uint)EventKind.RescanStarted, 0, "C:");
        await server.SendEventAsync((uint)EventKind.VolumeFailed, 0, "C:");
        await Task.Delay(100);
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
        Assert.Contains("area=pipe", log.Text, StringComparison.Ordinal);
        Assert.Contains("event=VolumeUpdated", log.Text, StringComparison.Ordinal);
        Assert.Contains("event handler failed", log.Text, StringComparison.Ordinal);
        Assert.Contains("error_type=System.InvalidOperationException", log.Text, StringComparison.Ordinal);
        Assert.Equal(
            4,
            log.Text.Split('\n').Count(line =>
                line.Contains("event=VolumeUpdated", StringComparison.Ordinal)));
    }

    [Fact]
    public async Task Every_nonvolume_event_logs_its_exact_name_when_a_subscriber_fails()
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.IndexChanged += _ => throw new InvalidOperationException("index consumer failed");
        client.EngineErrorOccurred += _ => throw new InvalidOperationException("error consumer failed");
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        await WaitUntilAsync(
            () => log.Text.Contains("event=IndexChanged", StringComparison.Ordinal),
            "handshake catch-up index event log");
        var initialIndexFailures = log.Text.Split('\n').Count(line =>
            line.Contains("event=IndexChanged", StringComparison.Ordinal));

        await server.SendEventAsync((uint)EventKind.IndexChanged, 0, "C:");
        await server.SendEventAsync((uint)EventKind.EngineError, 1, "C:");
        await WaitUntilAsync(
            () => log.Text.Contains("event=EngineErrorOccurred", StringComparison.Ordinal),
            "engine error event log");

        Assert.Equal(
            initialIndexFailures + 1,
            log.Text.Split('\n').Count(line =>
                line.Contains("event=IndexChanged", StringComparison.Ordinal)));
        Assert.Contains(
            log.Text.Split('\n'),
            line => line.Contains("event=IndexChanged", StringComparison.Ordinal)
                && line.Contains("event handler failed", StringComparison.Ordinal));
        Assert.Contains("event=EngineErrorOccurred", log.Text, StringComparison.Ordinal);
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
    }

    [Fact]
    public async Task Handshake_catchup_failures_log_the_exact_event_names()
    {
        using var log = new LogCapture();
        using var client = new PipeEngineClient("fmf-test-catchup", autoStart: false);
        client.VolumeUpdated += _ => throw new InvalidOperationException("volume catchup failed");
        client.IndexChanged += _ => throw new InvalidOperationException("index catchup failed");

        client.PublishHandshakeCatchUpForTests([new("C:", VolumeState.Ready, 1)]);

        Assert.Contains("event=VolumeUpdated", log.Text, StringComparison.Ordinal);
        Assert.Contains("event=IndexChanged", log.Text, StringComparison.Ordinal);
    }

    [Fact]
    public async Task Unknown_event_kind_retires_the_connection_and_recovers()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        await server.SendEventAsync(99, 0, "C:");

        await WaitUntilAsync(
            () => server.ConnectionCount >= 2
                && client.Connection == EngineConnectionState.Connected,
            "unknown event reconnect");
    }

    [Fact]
    public async Task Event_header_and_body_kind_mismatch_retires_the_connection()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        await server.SendEventAsync(
            (ushort)EventKind.Progress,
            (uint)EventKind.VolumeReady,
            1,
            "C:");

        await WaitUntilAsync(
            () => server.ConnectionCount >= 2
                && client.Connection == EngineConnectionState.Connected,
            "mismatched event reconnect");
    }

    [Fact]
    public async Task A_page_request_from_the_wrong_epoch_is_stale_before_write()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        await Assert.ThrowsAsync<StaleResultException>(() => client.FetchPageAsync(
            1,
            new EngineRequest.Page(0, 1),
            client.CurrentEpoch + 1,
            CancellationToken.None));
        Assert.DoesNotContain(PipeProtocol.Op.ResultPage, server.OpcodesOf(0));
    }

    [Fact]
    public async Task A_response_from_the_wrong_epoch_cannot_complete_a_current_request()
    {
        var response = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ListVolumes
                ? response.Task
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var request = client.ListVolumesAsync();
        await server.WaitForAsync(PipeProtocol.Op.ListVolumes);
        var idField = typeof(PipeEngineClient).GetField(
            "_requestId",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        var requestId = unchecked((uint)Assert.IsType<int>(idField?.GetValue(client)));
        client.DispatchResponseForTests(
            client.CurrentEpoch + 1,
            requestId,
            PipeProtocol.Op.ListVolumes,
            PipeProtocol.Status.Ok,
            PipeProtocol.EncodeVolumeStatuses(server.Statuses));

        Assert.False(request.IsCompleted);
        response.SetResult((
            PipeProtocol.Status.Ok,
            PipeProtocol.EncodeVolumeStatuses(server.Statuses)));
        Assert.Equal(["C:"], await request);
    }

    [Fact]
    public async Task Presentation_basis_epoch_race_retries_once_without_the_old_handle()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var basis = await client.SearchAsync("first", SearchOptions.Default);

        client.AfterBasisAcquireForTests = async () =>
        {
            server.DisconnectAll();
            await WaitUntilAsync(
                () => server.ConnectionCount >= 2
                    && client.Connection == EngineConnectionState.Connected,
                "replacement connection");
        };
        var outcome = await client.SearchAsync("second", SearchOptions.Default, basis.Result);

        var secondQuery = server.Received.Last(r => r.Opcode == PipeProtocol.Op.Query);
        Assert.Equal(0UL, PipeProtocol.DecodeQueryReq(secondQuery.Payload).PresentationBasis);
        var lifetimeField = basis.Result.GetType().GetField(
            "_lifetime",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        var lifetime = Assert.IsType<ResultLeaseGate>(lifetimeField?.GetValue(basis.Result));
        var activeField = typeof(ResultLeaseGate).GetField(
            "_active",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        Assert.Equal(0, Assert.IsType<int>(activeField?.GetValue(lifetime)));
        outcome.Result.Dispose();
        basis.Result.Dispose();
    }

    [Fact]
    public async Task Disposal_after_handshake_never_publishes_the_private_connection()
    {
        using var server = new FakePipeServer();
        var client = new PipeEngineClient(server.PipeName, autoStart: false);
        var reached = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var resume = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var states = new List<EngineConnectionState>();
        client.ConnectionChanged += states.Add;
        client.AfterHandshakeForTests = () =>
        {
            reached.SetResult();
            return resume.Task;
        };
        client.Start();
        await reached.Task.WaitAsync(TimeSpan.FromSeconds(5));

        client.Dispose();
        resume.SetResult();

        await WaitUntilAsync(
            () => server.ClosedConnectionCount >= 1,
            "cancelled private handshake connection disposal");
        Assert.DoesNotContain(EngineConnectionState.Connected, states);
    }

    [Fact]
    public async Task Cancellation_after_query_response_releases_the_unpublished_result()
    {
        using var server = new FakePipeServer { Rows = [Rows.File(1, "one.txt")] };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var reached = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var resume = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        client.AfterQueryResponseForTests = () =>
        {
            reached.SetResult();
            return resume.Task;
        };
        using var cancellation = new CancellationTokenSource();
        var request = client.SearchAsync("one", SearchOptions.Default, cancellation.Token);
        await reached.Task.WaitAsync(TimeSpan.FromSeconds(5));

        cancellation.Cancel();
        resume.SetResult();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => request);
        await server.WaitForAsync(PipeProtocol.Op.ResultFree);
    }

    [Fact]
    public async Task Disposing_with_a_pending_request_normalizes_lifetime_cancellation()
    {
        var held = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ListVolumes
                ? held.Task
                : null,
        };
        var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var request = client.ListVolumesAsync();
        await server.WaitForAsync(PipeProtocol.Op.ListVolumes);

        client.Dispose();

        var failure = await Assert.ThrowsAsync<EngineUnavailableException>(() => request);
        Assert.Equal("engine client disposed", failure.Message);
    }

    [Fact]
    public async Task Cancelled_query_with_malformed_late_response_is_contained()
    {
        using var log = new LogCapture();
        var response = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.Query ? response.Task : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        using var cts = new CancellationTokenSource();
        var search = client.SearchAsync("cancel", SearchOptions.Default, cts.Token);
        await server.WaitForAsync(PipeProtocol.Op.Query);
        cts.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => search);
        await server.WaitForAsync(PipeProtocol.Op.QueryCancel);

        response.SetResult((PipeProtocol.Status.Ok, []));
        await Task.Delay(100);
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
        Assert.DoesNotContain(PipeProtocol.Op.ResultFree, server.OpcodesOf(0));
        Assert.Contains("area=pipe", log.Text, StringComparison.Ordinal);
        Assert.Contains("late query result could not be released", log.Text, StringComparison.Ordinal);
    }

    [Theory]
    [InlineData(1, 1, false, true)]
    [InlineData(1, 2, false, false)]
    [InlineData(1, 1, true, false)]
    public void Query_cancel_targets_only_the_current_live_connection(
        int connectionEpoch,
        int currentEpoch,
        bool disposed,
        bool expected) =>
        Assert.Equal(
            expected,
            PipeEngineClient.ShouldSendQueryCancel(
                connectionEpoch,
                currentEpoch,
                disposed));

    [Fact]
    public async Task Query_cancel_is_dropped_after_the_captured_connection_is_retired()
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "initial connection");
        var stale = Assert.IsType<PipeConnection>(client.CurrentPipeConnectionForTests);

        server.DisconnectAll();
        await WaitUntilAsync(
            () => server.ConnectionCount >= 2
                && client.Connection == EngineConnectionState.Connected
                && client.CurrentEpoch != stale.Epoch,
            "replacement connection");

        var cancelsBefore = server.Received.Count(
            frame => frame.Opcode == PipeProtocol.Op.QueryCancel);
        client.SendQueryCancel(stale, 123);
        await Task.Delay(50);
        Assert.Equal(
            cancelsBefore,
            server.Received.Count(frame => frame.Opcode == PipeProtocol.Op.QueryCancel));
        Assert.DoesNotContain("area=pipe.query-cancel", log.Text, StringComparison.Ordinal);
    }

    [Fact]
    public async Task Query_cancel_is_sent_once_for_the_current_live_connection()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var connection = Assert.IsType<PipeConnection>(client.CurrentPipeConnectionForTests);

        client.SendQueryCancel(connection, 123);

        await server.WaitForAsync(PipeProtocol.Op.QueryCancel);
        Assert.Single(server.Received, frame => frame.Opcode == PipeProtocol.Op.QueryCancel);
    }

    [Fact]
    public async Task Empty_body_operations_emit_zero_length_payloads()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        await client.ListVolumesAsync();
        await client.GetStatusAsync();
        Assert.NotNull(await client.GetStatsAsync());
        Assert.Null(await client.TryGetServiceInfoAsync(CancellationToken.None));

        var connection = Assert.IsType<PipeConnection>(client.CurrentPipeConnectionForTests);
        client.SendQueryCancel(connection, 123);
        await server.WaitForAsync(PipeProtocol.Op.QueryCancel);

        ushort[] expected =
        [
            PipeProtocol.Op.Subscribe,
            PipeProtocol.Op.IndexStatus,
            PipeProtocol.Op.ListVolumes,
            PipeProtocol.Op.Stats,
            PipeProtocol.Op.ServiceInfo,
            PipeProtocol.Op.QueryCancel,
        ];
        var received = server.Received
            .Where(frame => expected.Contains(frame.Opcode))
            .ToArray();

        Assert.Equal(
            expected.Order(),
            received.Select(frame => frame.Opcode).Distinct().Order());
        Assert.All(received, frame => Assert.Empty(frame.Payload));
    }

    [Fact]
    public async Task Query_cancel_write_failure_keeps_its_diagnostic_area()
    {
        Notifier.ResetForTests();
        try
        {
            using var log = new LogCapture();
            var notification = new TaskCompletionSource<AppNotification>(
                TaskCreationOptions.RunContinuationsAsynchronously);
            using var subscription = Notifier.Attach(item => notification.TrySetResult(item));
            using var server = new FakePipeServer();
            using var client = new PipeEngineClient(server.PipeName, autoStart: false);
            client.Start();
            await WaitUntilAsync(
                () => client.Connection == EngineConnectionState.Connected,
                "connected");
            var connection = Assert.IsType<PipeConnection>(client.CurrentPipeConnectionForTests);
            var disposed = typeof(PipeConnection).GetField(
                "_disposed",
                System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
            disposed?.SetValue(connection, true);

            client.SendQueryCancel(connection, 123);

            var posted = await notification.Task.WaitAsync(TimeSpan.FromSeconds(5));
            Assert.Contains("pipe.query-cancel", posted.Message, StringComparison.Ordinal);
            Assert.Contains("area=pipe.query-cancel", log.Text, StringComparison.Ordinal);
        }
        finally
        {
            Notifier.ResetForTests();
        }
    }

    [Fact]
    public async Task Timeout_reason_is_preserved_for_other_requests_on_the_retired_epoch()
    {
        // The production code deliberately avoids capturing a context here. Keep
        // this transport-only seam context-free too, so mutation scheduling cannot
        // change the test's continuation semantics.
        SyncContext.RunContinuationsInline();
        var held = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ListVolumes
                ? held.Task
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false)
        {
            RequestTimeout = TimeSpan.FromMilliseconds(250),
        };
        var timeoutReached = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var allowRetire = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        client.BeforeTimeoutRetireForTests = () =>
        {
            timeoutReached.TrySetResult();
            return allowRetire.Task;
        };
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        var timingOut = client.ListVolumesAsync();
        await server.WaitForAsync(PipeProtocol.Op.ListVolumes);
        await timeoutReached.Task.WaitAsync(TimeSpan.FromSeconds(5));

        // The first request is now timed out but deliberately paused before it
        // retires the connection. Start the collateral request in that exact
        // window instead of approximating it with a wall-clock delay: mutation
        // runs can be heavily loaded, making a 125 ms delay resume after both
        // requests have already timed out.
        var collateral = client.ListVolumesAsync();
        await server.WaitForAsync(PipeProtocol.Op.ListVolumes, minCount: 2);
        allowRetire.SetResult();

        var expected = $"request (opcode {PipeProtocol.Op.ListVolumes}) timed out after 0s";
        Assert.Equal(
            expected,
            (await Assert.ThrowsAsync<EngineUnavailableException>(() => timingOut)).Message);
        Assert.Equal(
            expected,
            (await Assert.ThrowsAsync<EngineUnavailableException>(() => collateral)).Message);
    }

    [Fact]
    public async Task Completed_query_detaches_its_caller_cancellation_callback()
    {
        // See SyncContext.RunContinuationsInline: FakePipeServer's background
        // loops must not become xUnit-tracked work whose scheduling can turn an
        // otherwise equivalent ConfigureAwait mutation into a spurious kill.
        SyncContext.RunContinuationsInline();
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        using var cancellation = new CancellationTokenSource();
        var outcome = await client.SearchAsync(
            "completed",
            SearchOptions.Default,
            cancellation.Token);
        var cancels = server.Received.Count(
            frame => frame.Opcode == PipeProtocol.Op.QueryCancel);

        cancellation.Cancel();
        await Task.Delay(100);

        Assert.Equal(
            cancels,
            server.Received.Count(frame => frame.Opcode == PipeProtocol.Op.QueryCancel));
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task Caller_cancellation_cannot_interrupt_a_Query_frame_write()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var connection = Assert.IsType<PipeConnection>(client.CurrentPipeConnectionForTests);
        var lockField = typeof(PipeConnection).GetField(
            "_writeLock",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        var writeLock = Assert.IsType<SemaphoreSlim>(lockField?.GetValue(connection));
        await writeLock.WaitAsync();
        using var cancellation = new CancellationTokenSource();
        try
        {
            var search = client.SearchAsync(
                "blocked-write",
                SearchOptions.Default,
                cancellation.Token);
            cancellation.Cancel();
            await Task.Delay(50);
            Assert.False(search.IsCompleted);

            writeLock.Release();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => search);
        }
        finally
        {
            if (writeLock.CurrentCount == 0)
            {
                writeLock.Release();
            }
        }

        await server.WaitForAsync(PipeProtocol.Op.Query);
        await server.WaitForAsync(PipeProtocol.Op.QueryCancel);
    }

    [Fact]
    public async Task Caller_cancellation_interrupts_a_blocked_nonquery_write()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var connection = Assert.IsType<PipeConnection>(client.CurrentPipeConnectionForTests);
        var lockField = typeof(PipeConnection).GetField(
            "_writeLock",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        var writeLock = Assert.IsType<SemaphoreSlim>(lockField?.GetValue(connection));
        await writeLock.WaitAsync();
        using var cancellation = new CancellationTokenSource();
        try
        {
            var request = client.ListVolumesAsync(cancellation.Token);
            cancellation.Cancel();

            var completed = await Task.WhenAny(request, Task.Delay(200));
            Assert.Same(request, completed);
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => request);
            Assert.DoesNotContain(PipeProtocol.Op.ListVolumes, server.OpcodesOf(0));
        }
        finally
        {
            writeLock.Release();
        }
    }

    [Fact]
    public async Task Faulting_connection_subscriber_logs_the_exact_event_name()
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        client.ConnectionChanged += _ => throw new InvalidOperationException("state consumer failed");

        server.DisconnectAll();
        await WaitUntilAsync(
            () => log.Text.Contains("event=ConnectionChanged", StringComparison.Ordinal),
            "faulting connection event log");

        Assert.Contains("event handler failed", log.Text, StringComparison.Ordinal);
    }

    [Fact]
    public async Task Publishing_the_same_connection_state_is_idempotent()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var publications = 0;
        client.ConnectionChanged += _ => publications++;
        var setConnection = typeof(PipeEngineClient).GetMethod(
            "SetConnection",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);

        setConnection?.Invoke(client, [EngineConnectionState.Connected]);

        Assert.Equal(0, publications);
    }

    [Theory]
    [InlineData(PipeProtocol.Status.Stale)]
    [InlineData(PipeProtocol.Status.Io)]
    public async Task Result_release_cleanup_never_surfaces_server_failures(int status)
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ResultFree
                ? Task.FromResult((status, "release failed"u8.ToArray()))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        var release = typeof(PipeEngineClient).GetMethod(
            "ReleaseResultIfCurrentAsync",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        var releaseTask = Assert.IsAssignableFrom<Task>(
            release?.Invoke(client, [1UL, client.CurrentEpoch]));
        await releaseTask;
        await server.WaitForAsync(PipeProtocol.Op.ResultFree);
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
        Assert.Equal(
            status == PipeProtocol.Status.Io,
            log.Text.Split('\n').Any(line =>
                line.Contains("area=pipe", StringComparison.Ordinal)
                && line.Contains("result release failed", StringComparison.Ordinal)));
    }

    [Fact]
    public async Task Service_info_is_best_effort_while_disconnected()
    {
        using var log = new LogCapture();
        using var client = new PipeEngineClient("fmf-test-no-service-info", autoStart: false);

        Assert.Null(await client.TryGetServiceInfoAsync(CancellationToken.None));
        Assert.Contains("area=pipe", log.Text, StringComparison.Ordinal);
        Assert.Contains("service info unavailable", log.Text, StringComparison.Ordinal);
    }

    [Fact]
    public async Task Service_info_deserializes_a_successful_pipe_response()
    {
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ServiceInfo
                ? Task.FromResult((
                    PipeProtocol.Status.Ok,
                    """{"uptime_ms":123,"connections":2,"version":"1.2.3"}"""u8.ToArray()))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        var info = Assert.IsType<ServiceInfoData>(
            await client.TryGetServiceInfoAsync(CancellationToken.None));

        Assert.Equal(123UL, info.UptimeMs);
        Assert.Equal(2U, info.Connections);
        Assert.Equal("1.2.3", info.Version);
    }

    [Fact]
    public async Task Page_fetch_updates_the_transport_rtt_stat()
    {
        using var server = new FakePipeServer { Rows = [Rows.File(1, "one.txt")] };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var outcome = await client.SearchAsync("one", SearchOptions.Default);

        Assert.Single(await outcome.Result.GetRangeAsync(0, 1));
        var stats = Assert.IsType<EngineStatsData>(await client.GetStatsAsync());

        Assert.NotNull(stats.Transport);
        Assert.True(stats.Transport.PageRttEwmaUs > 0);
        outcome.Result.Dispose();
    }

    [Theory]
    [InlineData("")]
    [InlineData("{\"unchanged\":true}")]
    public async Task Query_trace_controls_deserialization_and_served_logging(string traceJson)
    {
        using var log = new LogCapture();
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.Query
                ? Task.FromResult((
                    PipeProtocol.Status.Ok,
                    PipeProtocol.EncodeQueryResp(7, 0, traceJson)))
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        var outcome = await client.SearchAsync("trace", SearchOptions.Default);

        Assert.Equal(traceJson.Length > 0, outcome.Trace is not null);
        Assert.Equal(
            traceJson.Length == 0,
            log.Text.Contains("query served", StringComparison.Ordinal));
        outcome.Result.Dispose();
    }

    [Fact]
    public async Task Releasing_a_result_from_a_retired_epoch_never_hits_the_replacement()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "first connection");
        var retiredEpoch = client.CurrentEpoch;

        server.DisconnectAll();
        await WaitUntilAsync(
            () => server.ConnectionCount >= 2
                && client.Connection == EngineConnectionState.Connected
                && client.CurrentEpoch != retiredEpoch,
            "replacement connection");
        var frees = server.Received.Count(frame => frame.Opcode == PipeProtocol.Op.ResultFree);

        client.ReleaseResult(99, retiredEpoch);
        await Task.Delay(100);

        Assert.Equal(
            frees,
            server.Received.Count(frame => frame.Opcode == PipeProtocol.Op.ResultFree));
    }

    [Fact]
    public void Disposing_an_unstarted_client_disposes_its_lifetime_token_source()
    {
        var client = new PipeEngineClient("fmf-test-unstarted", autoStart: false);
        var field = typeof(PipeEngineClient).GetField(
            "_cts",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        var lifetime = Assert.IsType<CancellationTokenSource>(field?.GetValue(client));

        client.Dispose();

        Assert.Throws<ObjectDisposedException>(() => _ = lifetime.Token);
        Assert.Null(Record.Exception(client.Dispose));
    }

    [Fact]
    public async Task Disposing_a_started_client_eventually_disposes_its_lifetime_token_source()
    {
        using var server = new FakePipeServer();
        var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var field = typeof(PipeEngineClient).GetField(
            "_cts",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
        var lifetime = Assert.IsType<CancellationTokenSource>(field?.GetValue(client));

        client.Dispose();

        await WaitUntilAsync(
            () => IsDisposed(lifetime),
            "client lifetime disposal");
        Assert.Null(Record.Exception(client.Dispose));
    }

    private static async Task WaitUntilAsync(Func<bool> predicate, string what)
    {
        var deadline = Environment.TickCount64 + 5000;
        while (!predicate())
        {
            if (Environment.TickCount64 > deadline)
            {
                throw new TimeoutException($"timed out waiting for {what}");
            }

            await Task.Delay(10);
        }
    }

    private static bool IsDisposed(CancellationTokenSource source)
    {
        try
        {
            _ = source.Token;
            return false;
        }
        catch (ObjectDisposedException)
        {
            return true;
        }
    }
}
