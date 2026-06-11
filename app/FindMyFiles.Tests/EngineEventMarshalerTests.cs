using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// <see cref="EngineEventMarshaler"/>: the single engine-thread → UI-thread
/// crossing. Deterministic via <see cref="ManualDispatcher"/> — events raised
/// "on the engine thread" must not reach handlers until the UI queue drains.
/// </summary>
public sealed class EngineEventMarshalerTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();

    [Fact]
    public void Events_AreDeferredToTheDispatcher_ThenReRaisedInOrderWithPayloads()
    {
        using var marshaler = new EngineEventMarshaler(_engine, _dispatcher);
        var log = new List<string>();
        marshaler.IndexChanged += v => log.Add($"index {v}");
        marshaler.VolumeUpdated += s => log.Add($"volume {s.Label} {s.State} {s.Entries}");
        marshaler.EngineErrorOccurred += s => log.Add($"error {s}");
        marshaler.ConnectionChanged += s => log.Add($"connection {s}");

        // Raised on the "engine thread": nothing reaches a handler until the
        // UI queue runs — the marshaling is the whole point.
        _engine.RaiseVolumeUpdated(new VolumeStatus("C:", VolumeState.Ready, 42));
        _engine.RaiseIndexChanged("C:");
        _engine.RaiseEngineError(2);
        _engine.RaiseConnectionChanged(EngineConnectionState.Reconnecting);
        Assert.Empty(log);

        // One drain delivers all four, payloads intact, FIFO order kept.
        _dispatcher.DrainQueue();
        Assert.Equal(
            ["volume C: Ready 42", "index C:", "error 2", "connection Reconnecting"],
            log);
    }

    [Fact]
    public void MultipleConsumers_ShareOneCrossing_PerUnderlyingEvent()
    {
        using var marshaler = new EngineEventMarshaler(_engine, _dispatcher);
        var first = 0;
        var second = 0;
        marshaler.IndexChanged += _ => first++;
        marshaler.IndexChanged += _ => second++;

        _engine.RaiseIndexChanged("*");
        // One TryEnqueue hop fans out to every subscriber synchronously.
        Assert.Equal(1, _dispatcher.DrainQueue());
        Assert.Equal(1, first);
        Assert.Equal(1, second);
    }

    [Fact]
    public void Dispose_Unsubscribes_NothingFlowsAfterwards()
    {
        var marshaler = new EngineEventMarshaler(_engine, _dispatcher);
        var delivered = 0;
        marshaler.IndexChanged += _ => delivered++;
        Assert.Equal(1, _engine.IndexChangedSubscribers);

        marshaler.Dispose();
        Assert.Equal(0, _engine.IndexChangedSubscribers);
        _engine.RaiseIndexChanged("C:");
        _dispatcher.DrainQueue();
        Assert.Equal(0, delivered);
    }
}
