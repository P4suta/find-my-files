using System.Diagnostics;
using System.IO.Pipes;
using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.Contract;
using FindMyFiles.Tests.TestDoubles;
using Microsoft.Win32.SafeHandles;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Failure/status/event branches of <see cref="PipeEngineClient"/> that are not part of the happy-path state-machine specification.</summary>
public sealed class PipeEngineClientAdditionalTests
{
    private sealed class CountingSyncContext : SynchronizationContext
    {
        private int _posted;

        public int Posted => Volatile.Read(ref _posted);

        public override void Post(SendOrPostCallback d, object? state)
        {
            Interlocked.Increment(ref _posted);
            Assert.True(ThreadPool.QueueUserWorkItem(
                static work => work.Callback(work.State),
                (Callback: d, State: state),
                preferLocal: false));
        }
    }

    [Fact]
    public async Task Post_write_hook_continues_without_capturing_its_ambient_context()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        var suspended = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var allowContinuation = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var context = new CountingSyncContext();
        var marker = new AsyncLocal<bool>(change =>
        {
            if (change.ThreadContextChanged && change.PreviousValue && !change.CurrentValue)
            {
                suspended.TrySetResult();
            }
        });
        client.AfterRequestWriteForTests = () =>
        {
            SynchronizationContext.SetSynchronizationContext(context);
            marker.Value = true;
            return allowContinuation.Task;
        };
        var cancellation = TestContext.Current.CancellationToken;
        var request = Task.Run(() => client.ListVolumesAsync(cancellation), cancellation);
        try
        {
            await suspended.Task.WaitAsync(TimeSpan.FromSeconds(5), cancellation);
            allowContinuation.SetResult();
            await request.WaitAsync(TimeSpan.FromSeconds(5), cancellation);
            Assert.Equal(0, context.Posted);
        }
        finally
        {
            client.AfterRequestWriteForTests = null;
            allowContinuation.TrySetResult();
            await request.WaitAsync(TimeSpan.FromSeconds(5), cancellation);
        }
    }

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
    public async Task Synchronous_probe_reports_success_and_failure_without_throwing()
    {
        using var server = new FakePipeServer();

        await Task.Factory.StartNew(
            () =>
            {
                Assert.False(Thread.CurrentThread.IsThreadPoolThread);
                Assert.True(PipeEngineClient.Probe(server.PipeName, TimeSpan.FromSeconds(2)));
                Assert.False(PipeEngineClient.Probe(
                    "fmf-test-no-such-sync-pipe",
                    TimeSpan.FromMilliseconds(100)));
            },
            TestContext.Current.CancellationToken,
            TaskCreationOptions.LongRunning,
            TaskScheduler.Default);
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
        SyncContext.RunContinuationsInline();
        using var log = new LogCapture();
        var rejectedPipeName = "fmf-test-rejected-" + Guid.NewGuid().ToString("N");
        using var rejectedServer = new NamedPipeServerStream(
            rejectedPipeName, PipeDirection.InOut, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
        SafePipeHandle? rejectedHandle = null;
        using (var rejected = new PipeEngineClient(
            rejectedPipeName,
            autoStart: false,
            verifyServerIdentity: true,
            handle =>
            {
                rejectedHandle = handle;
                return false;
            }))
        {
            rejected.Start();
            var supervisorField = typeof(PipeEngineClient).GetField(
                "_supervisor",
                System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
            var supervisor = Assert.IsAssignableFrom<Task>(supervisorField?.GetValue(rejected));
            await supervisor.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
            Assert.Equal(EngineConnectionState.Faulted, rejected.Connection);
            Assert.True(Assert.IsType<SafePipeHandle>(rejectedHandle).IsClosed);
            Assert.False(rejectedServer.IsConnected);
            var failure = await Assert.ThrowsAsync<EngineUnavailableException>(
                () => rejected.ListVolumesAsync(TestContext.Current.CancellationToken));
            Assert.Contains(
                $@"server on \\.\pipe\{rejectedPipeName}",
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

        Assert.Equal(["C:"], await client.ListVolumesAsync(TestContext.Current.CancellationToken));
        await client.StartIndexingAsync(["C:"], TestContext.Current.CancellationToken);
        Assert.Equal([new VolumeStatus("C:", VolumeState.Ready, 42)], await client.GetStatusAsync(TestContext.Current.CancellationToken));
        Assert.NotNull(await client.GetStatsAsync(TestContext.Current.CancellationToken));

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
        await Task.Delay(100, TestContext.Current.CancellationToken);

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
            async () => await client.ListVolumesAsync(TestContext.Current.CancellationToken));

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
        var outcome = await client.SearchAsync("one", SearchOptions.Default, TestContext.Current.CancellationToken);
        server.Handler = (opcode, _) => opcode == PipeProtocol.Op.ResultPage
            ? Task.FromResult((PipeProtocol.Status.Io, "detail"u8.ToArray()))
            : null;

        var failure = await Assert.ThrowsAsync<EngineException>(
            () => outcome.Result.GetRangeAsync(0, 1, TestContext.Current.CancellationToken));

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
        await Task.Delay(100, TestContext.Current.CancellationToken);

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
        Assert.Null(await connected.GetStatsAsync(TestContext.Current.CancellationToken));

        using var disconnected = new PipeEngineClient("fmf-test-not-started", autoStart: false);
        Assert.Null(await disconnected.GetStatsAsync(TestContext.Current.CancellationToken));
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

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task Recoverable_handshake_failures_observe_the_initial_retry_backoff(
        bool deferObservationUntilRetry)
    {
        var firstAttempt = new TaskCompletionSource<(long Timestamp, int Connections)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var retryAttempt = new TaskCompletionSource<(long Timestamp, int Connections)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var retryResponse = (PipeProtocol.Status.Io, "retry"u8.ToArray());
        var releaseRetry = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var handshakes = 0;
        using var server = new FakePipeServer();
        server.Handler = (opcode, _) =>
        {
            if (opcode != PipeProtocol.Op.Subscribe)
            {
                return null;
            }

            var observation = (Stopwatch.GetTimestamp(), server.ConnectionCount);
            if (Interlocked.Increment(ref handshakes) == 1)
            {
                firstAttempt.TrySetResult(observation);
                return Task.FromResult(retryResponse);
            }

            retryAttempt.TrySetResult(observation);
            return releaseRetry.Task;
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        try
        {
            if (deferObservationUntilRetry)
            {
                await retryAttempt.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
            }

            var first = await firstAttempt.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
            var retry = await retryAttempt.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
            Assert.Equal(1, first.Connections);
            Assert.Equal(2, retry.Connections);
            Assert.InRange(
                Stopwatch.GetElapsedTime(first.Timestamp, retry.Timestamp),
                TimeSpan.FromMilliseconds(100),
                TimeSpan.MaxValue);
            Assert.Equal(EngineConnectionState.Connecting, client.Connection);
        }
        finally
        {
            releaseRetry.TrySetResult(retryResponse);
        }
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

        await Task.Delay(100, TestContext.Current.CancellationToken);

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
        await Task.Delay(100, TestContext.Current.CancellationToken);
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
        var request = client.ListVolumesAsync(TestContext.Current.CancellationToken);
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
        var basis = await client.SearchAsync("first", SearchOptions.Default, TestContext.Current.CancellationToken);

        client.AfterBasisAcquireForTests = async () =>
        {
            server.DisconnectAll();
            await WaitUntilAsync(
                () => server.ConnectionCount >= 2
                    && client.Connection == EngineConnectionState.Connected,
                "replacement connection");
        };
        var outcome = await client.SearchAsync("second", SearchOptions.Default, basis.Result, TestContext.Current.CancellationToken);

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
        await reached.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);

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
        await reached.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);

        cancellation.Cancel();
        resume.SetResult();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => request);
        await server.WaitForAsync(PipeProtocol.Op.ResultFree);
    }

    [Theory]
    [InlineData(false, false)]
    [InlineData(false, true)]
    [InlineData(true, false)]
    [InlineData(true, true)]
    public async Task Cancellation_and_disposal_normalize_an_already_completed_connection_failure(
        bool cancelCaller,
        bool disposeClient)
    {
        var held = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ListVolumes
                ? held.Task
                : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");

        var written = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var allowWait = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        client.AfterRequestWriteForTests = () =>
        {
            written.TrySetResult();
            return allowWait.Task;
        };
        using var cancellation = new CancellationTokenSource();
        var request = client.ListVolumesAsync(cancellation.Token);
        try
        {
            await written.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
            await server.WaitForAsync(PipeProtocol.Op.ListVolumes);
            client.AfterRequestWriteForTests = null;

            if (cancelCaller)
            {
                cancellation.Cancel();
            }

            if (disposeClient)
            {
                client.Dispose();
            }
            else
            {
                var cleanupObserved = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
                client.ConnectionChanged += state =>
                {
                    if (state == EngineConnectionState.Reconnecting)
                    {
                        cleanupObserved.TrySetResult();
                    }
                };
                server.DisconnectAll();
                await cleanupObserved.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
            }

            allowWait.SetResult();

            if (cancelCaller)
            {
                var failure = await Assert.ThrowsAnyAsync<OperationCanceledException>(() => request);
                Assert.Equal(cancellation.Token, failure.CancellationToken);
            }
            else
            {
                var failure = await Assert.ThrowsAsync<EngineUnavailableException>(() => request);
                Assert.Equal(
                    disposeClient ? "engine client disposed" : "engine service connection lost",
                    failure.Message);
            }
        }
        finally
        {
            client.AfterRequestWriteForTests = null;
            allowWait.TrySetResult();
            client.Dispose();
            held.TrySetCanceled(TestContext.Current.CancellationToken);
            await ((Task)request).ConfigureAwait(
                ConfigureAwaitOptions.ContinueOnCapturedContext | ConfigureAwaitOptions.SuppressThrowing);
        }
    }

    [Theory]
    [InlineData(false, false, false)]
    [InlineData(false, false, true)]
    [InlineData(false, true, false)]
    [InlineData(false, true, true)]
    [InlineData(true, false, false)]
    [InlineData(true, false, true)]
    [InlineData(true, true, false)]
    [InlineData(true, true, true)]
    public async Task Caller_cancellation_keeps_its_token_before_and_during_response_wait(
        bool queryRequest,
        bool cancelBeforeWait,
        bool disposeClient)
    {
        var clock = new ObservedTimeProvider();
        var held = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var opcode = queryRequest ? PipeProtocol.Op.Query : PipeProtocol.Op.ListVolumes;
        using var server = new FakePipeServer
        {
            Handler = (operation, _) => operation == opcode ? held.Task : null,
        };
        using var client = new PipeEngineClient(server.PipeName, autoStart: false, requestTimeProvider: clock);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var written = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var allowWait = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var waiting = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        clock.TimerCreated = () => waiting.TrySetResult();
        client.AfterRequestWriteForTests = () =>
        {
            written.TrySetResult();
            return allowWait.Task;
        };
        using var cancellation = new CancellationTokenSource();
        Task request = queryRequest
            ? client.SearchAsync("pending", SearchOptions.Default, cancellation.Token)
            : client.ListVolumesAsync(cancellation.Token);
        try
        {
            await written.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
            if (cancelBeforeWait)
            {
                cancellation.Cancel();
                allowWait.SetResult();
            }
            else
            {
                allowWait.SetResult();
                await waiting.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
                cancellation.Cancel();
            }

            if (disposeClient)
            {
                client.Dispose();
            }

            var completed = await Task.WhenAny(
                request,
                Task.Delay(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken));
            Assert.Same(request, completed);
            var failure = await Assert.ThrowsAnyAsync<OperationCanceledException>(() => request);
            Assert.Equal(cancellation.Token, failure.CancellationToken);
        }
        finally
        {
            client.AfterRequestWriteForTests = null;
            clock.TimerCreated = null;
            allowWait.TrySetResult();
            await cancellation.CancelAsync();
            held.TrySetCanceled(cancellation.Token);
            clock.Advance(client.RequestTimeout);
            await request.ConfigureAwait(
                ConfigureAwaitOptions.ContinueOnCapturedContext | ConfigureAwaitOptions.SuppressThrowing);
        }
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
        var request = client.ListVolumesAsync(TestContext.Current.CancellationToken);
        await server.WaitForAsync(PipeProtocol.Op.ListVolumes);

        client.Dispose();

        var failure = await Assert.ThrowsAsync<EngineUnavailableException>(() => request);
        Assert.Equal("engine client disposed", failure.Message);
    }

    [Fact]
    public async Task Replacement_readiness_waits_while_retirement_has_cleared_the_slot()
    {
        SyncContext.RunContinuationsInline();
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        client.Start();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected,
            "connected");
        var epochBefore = client.CurrentEpoch;
        var cleared = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var allowRetirement = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        client.AfterRetiredConnectionDisposedForTests = () =>
        {
            cleared.TrySetResult();
            allowRetirement.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken)
                .GetAwaiter().GetResult();
        };
        server.DisconnectAll();
        await cleared.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        try
        {
            Assert.Equal(0, client.CurrentEpoch);
            Assert.Equal(EngineConnectionState.Connected, client.Connection);
            Assert.False(PipeOverFakeServerContractTests.IsPublishedReplacement(client, epochBefore));
        }
        finally
        {
            allowRetirement.TrySetResult();
        }

        await PipeOverFakeServerContractTests.WaitForPublishedReplacementAsync(client, epochBefore);
        Assert.NotNull(client.CurrentPipeConnectionForTests);
        Assert.NotEqual(epochBefore, client.CurrentEpoch);
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
        await Task.Delay(100, TestContext.Current.CancellationToken);
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
                && PipeOverFakeServerContractTests.IsPublishedReplacement(client, stale.Epoch),
            "replacement connection");

        var cancelsBefore = server.Received.Count(
            frame => frame.Opcode == PipeProtocol.Op.QueryCancel);
        client.SendQueryCancel(stale, 123);
        await Task.Delay(50, TestContext.Current.CancellationToken);
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

        await client.ListVolumesAsync(TestContext.Current.CancellationToken);
        await client.GetStatusAsync(TestContext.Current.CancellationToken);
        Assert.NotNull(await client.GetStatsAsync(TestContext.Current.CancellationToken));
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

            var posted = await notification.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
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
        SyncContext.RunContinuationsInline();
        var clock = new ObservedTimeProvider();
        var held = new TaskCompletionSource<(int Status, byte[] Payload)>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var server = new FakePipeServer
        {
            Handler = (opcode, _) => opcode == PipeProtocol.Op.ListVolumes
                ? held.Task
                : null,
        };
        using var client = new PipeEngineClient(
            server.PipeName,
            autoStart: false,
            requestTimeProvider: clock)
        {
            RequestTimeout = TimeSpan.FromMilliseconds(250),
        };
        var timeoutReached = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var allowRetire = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var cleanupObserved = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancellation = CancellationTokenSource.CreateLinkedTokenSource(
            TestContext.Current.CancellationToken);
        Task? timingOut = null;
        Task? collateral = null;
        try
        {
            client.BeforeTimeoutRetireForTests = () =>
            {
                timeoutReached.TrySetResult();
                return allowRetire.Task;
            };
            client.Start();
            await WaitUntilAsync(
                () => client.Connection == EngineConnectionState.Connected,
                "connected");

            var firstArmed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            clock.TimerCreated = () => firstArmed.TrySetResult();
            timingOut = client.ListVolumesAsync(cancellation.Token);
            await firstArmed.Task.WaitAsync(TimeSpan.FromSeconds(5), cancellation.Token);
            await server.WaitForAsync(PipeProtocol.Op.ListVolumes);
            clock.Advance(client.RequestTimeout);
            await timeoutReached.Task.WaitAsync(TimeSpan.FromSeconds(5), cancellation.Token);

            var secondArmed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            clock.TimerCreated = () => secondArmed.TrySetResult();
            collateral = client.ListVolumesAsync(cancellation.Token);
            await secondArmed.Task.WaitAsync(TimeSpan.FromSeconds(5), cancellation.Token);
            await server.WaitForAsync(PipeProtocol.Op.ListVolumes, minCount: 2);
            var pendingField = typeof(PipeEngineClient).GetField(
                "_pending",
                System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);
            var pending = Assert.IsAssignableFrom<System.Collections.IDictionary>(pendingField?.GetValue(client));
            var request = Assert.Single(pending.Values.Cast<object>());
            var completion = Assert.IsType<TaskCompletionSource<(int Status, byte[] Payload)>>(
                request.GetType().GetProperty("Completion")?.GetValue(request));

            client.ConnectionChanged += state =>
            {
                if (state == EngineConnectionState.Reconnecting)
                {
                    cleanupObserved.TrySetResult();
                }
            };
            var cleanupForced = false;
            client.AfterRetiredConnectionDisposedForTests = () =>
            {
                cleanupObserved.Task
                    .WaitAsync(TimeSpan.FromSeconds(5), cancellation.Token)
                    .GetAwaiter().GetResult();
                cleanupForced = true;
            };
            allowRetire.SetResult();

            var expected = $"request (opcode {PipeProtocol.Op.ListVolumes}) timed out after 0s";
            Assert.Equal(
                expected,
                (await Assert.ThrowsAsync<EngineUnavailableException>(() => timingOut)).Message);
            Assert.True(completion.Task.IsFaulted);
            Assert.Equal(
                expected,
                (await Assert.ThrowsAsync<EngineUnavailableException>(() => collateral)).Message);
            Assert.True(cleanupForced);
        }
        finally
        {
            client.BeforeTimeoutRetireForTests = null;
            client.AfterRetiredConnectionDisposedForTests = null;
            clock.TimerCreated = null;
            cleanupObserved.TrySetResult();
            allowRetire.TrySetResult();
            await cancellation.CancelAsync();
            held.TrySetCanceled(cancellation.Token);
            clock.Advance(client.RequestTimeout);
            if (timingOut is not null)
            {
                await timingOut.ConfigureAwait(
                    ConfigureAwaitOptions.ContinueOnCapturedContext | ConfigureAwaitOptions.SuppressThrowing);
            }

            if (collateral is not null)
            {
                await collateral.ConfigureAwait(
                    ConfigureAwaitOptions.ContinueOnCapturedContext | ConfigureAwaitOptions.SuppressThrowing);
            }
        }
    }

    [Fact]
    public async Task Completed_query_detaches_its_caller_cancellation_callback()
    {
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
        await Task.Delay(100, TestContext.Current.CancellationToken);

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
        await writeLock.WaitAsync(TestContext.Current.CancellationToken);
        using var cancellation = new CancellationTokenSource();
        var ownsWriteLock = true;
        var replacementWriterOwnsLock = false;
        try
        {
            var search = client.SearchAsync(
                "blocked-write",
                SearchOptions.Default,
                cancellation.Token);
            cancellation.Cancel();
            await Task.Delay(50, TestContext.Current.CancellationToken);
            Assert.False(search.IsCompleted);

            ownsWriteLock = false;
            writeLock.Release();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => search);
            await server.WaitForAsync(PipeProtocol.Op.Query);
            await server.WaitForAsync(PipeProtocol.Op.QueryCancel);
            using var handoffTimeout = new CancellationTokenSource(TimeSpan.FromSeconds(5));
            await writeLock.WaitAsync(handoffTimeout.Token);
            replacementWriterOwnsLock = true;
        }
        finally
        {
            try
            {
                if (ownsWriteLock)
                {
                    writeLock.Release();
                }

                if (replacementWriterOwnsLock)
                {
                    Assert.Equal(0, writeLock.CurrentCount);
                }
            }
            finally
            {
                if (replacementWriterOwnsLock && writeLock.CurrentCount == 0)
                {
                    writeLock.Release();
                }
            }
        }
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
        await writeLock.WaitAsync(TestContext.Current.CancellationToken);
        using var cancellation = new CancellationTokenSource();
        try
        {
            var request = client.ListVolumesAsync(cancellation.Token);
            cancellation.Cancel();

            var completed = await Task.WhenAny(request, Task.Delay(200, TestContext.Current.CancellationToken));
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
        var connected = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        client.ConnectionChanged += state =>
        {
            if (state == EngineConnectionState.Connected)
            {
                connected.TrySetResult();
            }
        };

        client.Start();
        await connected.Task.WaitAsync(TimeSpan.FromSeconds(5), TestContext.Current.CancellationToken);
        Assert.Equal(EngineConnectionState.Connected, client.Connection);
        var publications = 0;
        client.ConnectionChanged += _ => publications++;
        var setConnection = typeof(PipeEngineClient).GetMethod(
            "SetConnection",
            System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic);

        Assert.NotNull(setConnection);
        setConnection.Invoke(client, [EngineConnectionState.Connected]);

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
        var outcome = await client.SearchAsync("one", SearchOptions.Default, TestContext.Current.CancellationToken);

        Assert.Single(await outcome.Result.GetRangeAsync(0, 1, TestContext.Current.CancellationToken));
        var stats = Assert.IsType<EngineStatsData>(await client.GetStatsAsync(TestContext.Current.CancellationToken));

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

        var outcome = await client.SearchAsync("trace", SearchOptions.Default, TestContext.Current.CancellationToken);

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
                && PipeOverFakeServerContractTests.IsPublishedReplacement(client, retiredEpoch),
            "replacement connection");
        var frees = server.Received.Count(frame => frame.Opcode == PipeProtocol.Op.ResultFree);

        client.ReleaseResult(99, retiredEpoch);
        await Task.Delay(100, TestContext.Current.CancellationToken);

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
