using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary> Defense-in-depth behavior tests for the two "boring" UI-thread plumbing pieces whose simplest state transitions the thick existing suites skipped:
/// <see cref="EngineEventMarshaler"/> (does a repeated event still cross?
/// does a hop already queued before Dispose still land?)
/// and <see cref="NotificationCenter"/> (does a held Warning banner — the reconnect banner — survive a positional cap-overflow and stay removable?).
/// All deterministic via <see cref="ManualDispatcher"/>; outcomes are observable,
/// not internal.
/// </summary>
public sealed class EventMarshalingNotificationTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly StubEngineClient _engine = new();
    private readonly NotificationCenter _center;

    public EventMarshalingNotificationTests()
    {
        _center = new NotificationCenter(_dispatcher);
    }

    [Fact]
    public void RepeatedIdenticalEvent_StillMarshalsEachTime_NoDedupe()
    {
        using var marshaler = new EngineEventMarshaler(_engine, _dispatcher);
        var deliveries = new List<string>();
        marshaler.IndexChanged += v => deliveries.Add(v);

        _engine.RaiseIndexChanged("C:");
        _engine.RaiseIndexChanged("C:");

        Assert.Equal(2, _dispatcher.DrainQueue());
        Assert.Equal(["C:", "C:"], deliveries);
    }

    [Fact]
    public void EventEnqueuedBeforeDispose_StillDelivers_DisposeOnlyDetachesUpstream()
    {
        var marshaler = new EngineEventMarshaler(_engine, _dispatcher);
        var delivered = new List<string>();
        marshaler.IndexChanged += v => delivered.Add(v);

        _engine.RaiseIndexChanged("C:");
        marshaler.Dispose();

        _engine.RaiseIndexChanged("D:");
        Assert.Equal(0, _engine.IndexChangedSubscribers);

        _dispatcher.DrainQueue();
        Assert.Equal(["C:"], delivered);
    }

    [Fact]
    public void HeldWarning_SurvivesCapOverflow_OfTransientErrors_AndStaysRemovable()
    {
        var reconnect = new AppNotification(NotifySeverity.Warning, "reconnecting");
        _center.Push(new AppNotification(NotifySeverity.Error, "e0"));
        _center.Push(reconnect);
        _center.Push(new AppNotification(NotifySeverity.Error, "e1"));

        _center.Push(new AppNotification(NotifySeverity.Error, "e2"));

        Assert.Equal(3, _center.Items.Count);
        Assert.Contains(reconnect, _center.Items);
        Assert.DoesNotContain(_center.Items, n => string.Equals(n.Message, "e0", StringComparison.Ordinal));

        _center.Remove(reconnect);
        Assert.DoesNotContain(reconnect, _center.Items);
        Assert.Equal(2, _center.Items.Count);
    }
}
