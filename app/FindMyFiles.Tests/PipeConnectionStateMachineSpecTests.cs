using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests;

/// <summary>Exercises connection transitions and their ordered notifications against a named-pipe peer.</summary>
public sealed class PipeConnectionStateMachineSpecTests
{
    /// <summary>Thread-safe recorder of the emitted ConnectionChanged sequence (events fire on the supervisor / read-loop thread).</summary>
    private sealed class Recorder
    {
        private readonly System.Threading.Lock _gate = new();
        private readonly List<EngineConnectionState> _events = [];

        public void Attach(PipeEngineClient client) =>
            client.ConnectionChanged += s =>
            {
                lock (_gate)
                {
                    _events.Add(s);
                }
            };

        public Task WaitForCountAsync(int minimum) =>
            WaitUntilAsync(() => Snapshot().Length >= minimum, $"connection event count {minimum}");

        public EngineConnectionState[] Snapshot()
        {
            lock (_gate)
            {
                return [.. _events];
            }
        }
    }

    [Fact]
    public void Initial_state_is_Connecting_before_any_io()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);

        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
    }

    [Fact]
    public async Task Connecting_to_Connected_emits_Connected_exactly_once()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        await events.WaitForCountAsync(1);
        Assert.Equal([EngineConnectionState.Connected], events.Snapshot());
    }

    [Fact]
    public async Task WarmUp_self_loops_on_Connecting_silently_then_Connected()
    {
        using var server = new FakePipeServer(startHeld: true);
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await Task.Delay(400, TestContext.Current.CancellationToken);

        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
        Assert.Empty(events.Snapshot());

        server.ReleaseAccept();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        await events.WaitForCountAsync(1);
        Assert.Equal([EngineConnectionState.Connected], events.Snapshot());
    }

    [Fact]
    public async Task Connected_drop_then_recover_emits_Reconnecting_then_Connected()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "first connect");

        server.DisconnectAll();
        await WaitUntilAsync(
            () => server.ConnectionCount == 2 && client.Connection == EngineConnectionState.Connected,
            "reconnected");

        await events.WaitForCountAsync(3);
        Assert.Equal(
            new[]
            {
                EngineConnectionState.Connected,
                EngineConnectionState.Reconnecting,
                EngineConnectionState.Connected,
            },
            events.Snapshot());
    }

    [Fact]
    public async Task Connected_then_server_gone_settles_in_Reconnecting()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        server.Dispose();
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Reconnecting, "noticed the drop");

        await events.WaitForCountAsync(2);
        await Task.Delay(300, TestContext.Current.CancellationToken);
        Assert.Equal(EngineConnectionState.Reconnecting, client.Connection);
        Assert.Equal(
            [EngineConnectionState.Connected, EngineConnectionState.Reconnecting],
            events.Snapshot());
    }

    [Fact]
    public async Task Fatal_protocol_mismatch_is_terminal_and_never_Connected()
    {
        using var server = new FakePipeServer { ProtocolVersion = PipeProtocol.ProtocolVersion + 1 };
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await server.WaitForAsync(PipeProtocol.Op.Hello);
        await events.WaitForCountAsync(1);
        await Task.Delay(750, TestContext.Current.CancellationToken);

        Assert.Equal(EngineConnectionState.Faulted, client.Connection);
        Assert.DoesNotContain(EngineConnectionState.Connected, events.Snapshot());
        Assert.Equal([EngineConnectionState.Faulted], events.Snapshot());
        Assert.Equal(1, server.ConnectionCount);
    }

    [Fact]
    public async Task Fatal_mismatch_after_first_success_leaves_Connected_for_Faulted()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        server.ProtocolVersion = PipeProtocol.ProtocolVersion + 1;
        server.DisconnectAll();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Faulted, "faulted");

        await events.WaitForCountAsync(3);
        Assert.Equal(
            new[]
            {
                EngineConnectionState.Connected,
                EngineConnectionState.Reconnecting,
                EngineConnectionState.Faulted,
            },
            events.Snapshot());
        Assert.Equal(2, server.ConnectionCount);

        await Task.Delay(750, TestContext.Current.CancellationToken);
        Assert.Equal(2, server.ConnectionCount);
        var failure = await Assert.ThrowsAsync<EngineUnavailableException>(
            () => client.SearchAsync("a", SearchOptions.Default, TestContext.Current.CancellationToken));
        Assert.Contains("protocol", failure.Message, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public async Task ConnectionChanged_never_emits_the_same_state_twice_in_a_row()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connect");
        server.DisconnectAll();
        await WaitUntilAsync(
            () => server.ConnectionCount == 2 && client.Connection == EngineConnectionState.Connected,
            "reconnect");

        await events.WaitForCountAsync(3);
        var seq = events.Snapshot();
        Assert.Equal(3, seq.Length);
        for (var i = 1; i < seq.Length; i++)
        {
            Assert.NotEqual(seq[i - 1], seq[i]);
        }

        Assert.All(
            seq,
            s => Assert.Contains(
                s,
                new[] { EngineConnectionState.Connected, EngineConnectionState.Reconnecting }));
    }
}
