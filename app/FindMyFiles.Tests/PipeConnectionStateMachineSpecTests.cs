using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;
using static FindMyFiles.Tests.TestDoubles.Polling;

namespace FindMyFiles.Tests;

/// <summary>
/// The canonical, executable specification of the pipe client's connection state
/// machine. Each edge below is PROVEN by a test that drives the real
/// <see cref="PipeEngineClient"/> supervisor against a <see cref="FakePipeServer"/>
/// and asserts both the resulting <see cref="EngineConnectionState"/> and the exact
/// ordered sequence of <c>ConnectionChanged</c> events it emits. The shipped
/// onboarding bug was a transition (Connecting while the freshly-started service is
/// still warming up → Connected) that no test observed; this file makes every edge
/// a named, event-exact obligation, so a gap or a wrong emission fails loudly.
///
/// Transition table (this IS the spec — add a row AND a test for any new edge):
/// <code>
///   (initial)                                  → Connecting        [no event]
///   Connecting    + handshake succeeds         → Connected         [emit Connected]
///   Connecting    + connect fails (warm-up)    → Connecting        [no event — retry]
///   Connecting    + fatal (proto/identity)     → (terminal)        [no event, supervisor stops]
///   Connected     + connection drops           → Reconnecting      [emit Reconnecting]
///   Reconnecting  + handshake succeeds         → Connected         [emit Connected]
///   Reconnecting  + connect fails              → Reconnecting      [no event — retry]
/// </code>
/// Invariant proven across the suite: <c>ConnectionChanged</c> never emits the same
/// state twice in a row (the Connecting/Reconnecting retry self-loops are silent),
/// and Connected is only ever reached from Connecting (first time) or Reconnecting.
/// </summary>
public sealed class PipeConnectionStateMachineSpecTests
{
    /// <summary>Thread-safe recorder of the emitted ConnectionChanged sequence
    /// (events fire on the supervisor / read-loop thread).</summary>
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

        public EngineConnectionState[] Snapshot()
        {
            lock (_gate)
            {
                return [.. _events];
            }
        }
    }

    // ── (initial) → Connecting ──────────────────────────────────────────
    [Fact]
    public void Initial_state_is_Connecting_before_any_io()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);

        // The machine starts in Connecting before the supervisor even runs — the
        // precondition every Connected is reached from.
        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
    }

    // ── Connecting + handshake succeeds → Connected [emit Connected] ─────
    [Fact]
    public async Task Connecting_to_Connected_emits_Connected_exactly_once()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        Assert.Equal([EngineConnectionState.Connected], events.Snapshot());
    }

    // ── Connecting + connect fails (warm-up) → Connecting [no event] ─────
    [Fact]
    public async Task WarmUp_self_loops_on_Connecting_silently_then_Connected()
    {
        using var server = new FakePipeServer(startHeld: true); // not serving yet
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await Task.Delay(400); // several connect attempts fail in this window

        // The retry self-loop is SILENT: still Connecting, and not one event yet.
        Assert.Equal(EngineConnectionState.Connecting, client.Connection);
        Assert.Empty(events.Snapshot());

        server.ReleaseAccept(); // service starts serving
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        // The ONLY emission across the whole warm-up is the single Connected.
        Assert.Equal([EngineConnectionState.Connected], events.Snapshot());
    }

    // ── Connected + drop → Reconnecting → Connected ─────────────────────
    [Fact]
    public async Task Connected_drop_then_recover_emits_Reconnecting_then_Connected()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "first connect");

        server.DisconnectAll(); // the live connection drops; the server stays up
        await WaitUntilAsync(
            () => server.ConnectionCount == 2 && client.Connection == EngineConnectionState.Connected,
            "reconnected");

        // The full edge sequence, in order — this is the load-bearing assertion.
        Assert.Equal(
            new[]
            {
                EngineConnectionState.Connected,
                EngineConnectionState.Reconnecting,
                EngineConnectionState.Connected,
            },
            events.Snapshot());
    }

    // ── Connected + server gone → Reconnecting (stays) ──────────────────
    [Fact]
    public async Task Connected_then_server_gone_settles_in_Reconnecting()
    {
        using var server = new FakePipeServer();
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await WaitUntilAsync(() => client.Connection == EngineConnectionState.Connected, "connected");

        server.Dispose(); // server gone for good — no reconnection possible
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Reconnecting, "noticed the drop");

        await Task.Delay(300); // it must stay Reconnecting, not bounce back to Connected
        Assert.Equal(EngineConnectionState.Reconnecting, client.Connection);
        Assert.Equal(
            [EngineConnectionState.Connected, EngineConnectionState.Reconnecting],
            events.Snapshot());
    }

    // ── Connecting + fatal → terminal (never Connected, supervisor stops) ─
    [Fact]
    public async Task Fatal_protocol_mismatch_is_terminal_and_never_Connected()
    {
        using var server = new FakePipeServer { ProtocolVersion = PipeProtocol.ProtocolVersion + 1 };
        var events = new Recorder();
        using var client = new PipeEngineClient(server.PipeName, autoStart: false);
        events.Attach(client);

        client.Start();
        await server.WaitForAsync(PipeProtocol.Op.Hello);
        await Task.Delay(750); // a non-fatal failure would have retried by now

        Assert.NotEqual(EngineConnectionState.Connected, client.Connection);
        Assert.DoesNotContain(EngineConnectionState.Connected, events.Snapshot());
        Assert.Equal(1, server.ConnectionCount); // stopped: no retry storm
    }

    // ── Invariant: no state is ever emitted twice in a row (dedup) ──────
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

        var seq = events.Snapshot();
        for (var i = 1; i < seq.Length; i++)
        {
            Assert.NotEqual(seq[i - 1], seq[i]); // SetConnection collapses same-state churn
        }

        // Closure: every emitted state is one of the two transition targets; the
        // third reachable state, Connecting, is the silent initial/self-loop.
        Assert.All(
            seq,
            s => Assert.Contains(
                s,
                new[] { EngineConnectionState.Connected, EngineConnectionState.Reconnecting }));
    }
}
