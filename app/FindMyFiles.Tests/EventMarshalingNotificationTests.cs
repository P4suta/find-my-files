using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Defense-in-depth behavior tests for the two "boring" UI-thread plumbing
/// pieces whose simplest state transitions the thick existing suites skipped:
/// <see cref="EngineEventMarshaler"/> (does a repeated event still cross? does a
/// hop already queued before Dispose still land?) and
/// <see cref="NotificationCenter"/> (does a held Warning banner — the reconnect
/// banner — survive a positional cap-overflow and stay removable?). All
/// deterministic via <see cref="ManualDispatcher"/>; outcomes are observable,
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

        // Same payload twice on the "engine thread".
        _engine.RaiseIndexChanged("C:");
        _engine.RaiseIndexChanged("C:");

        // The marshaler does NOT collapse duplicates: two raises = two crossings
        // = two handler calls. (Documenting actual behavior — there is no dedupe.)
        Assert.Equal(2, _dispatcher.DrainQueue());
        Assert.Equal(["C:", "C:"], deliveries);
    }

    [Fact]
    public void EventEnqueuedBeforeDispose_StillDelivers_DisposeOnlyDetachesUpstream()
    {
        var marshaler = new EngineEventMarshaler(_engine, _dispatcher);
        var delivered = new List<string>();
        marshaler.IndexChanged += v => delivered.Add(v);

        // A hop is marshaled but not yet drained, then we Dispose.
        _engine.RaiseIndexChanged("C:");
        marshaler.Dispose();

        // Post-Dispose the upstream subscription is gone, so this raise queues
        // nothing — but the already-queued "C:" hop is untouched by Dispose and
        // still lands when the UI queue runs.
        _engine.RaiseIndexChanged("D:");
        Assert.Equal(0, _engine.IndexChangedSubscribers);

        _dispatcher.DrainQueue();
        Assert.Equal(["C:"], delivered);
    }

    [Fact]
    public void HeldWarning_SurvivesCapOverflow_OfTransientErrors_AndStaysRemovable()
    {
        // The reconnect banner: a Warning that must persist while transient
        // errors churn underneath it. It is not the oldest entry — an earlier
        // transient error is.
        var reconnect = new AppNotification(NotifySeverity.Warning, "reconnecting");
        _center.Push(new AppNotification(NotifySeverity.Error, "e0")); // oldest
        _center.Push(reconnect);
        _center.Push(new AppNotification(NotifySeverity.Error, "e1"));

        // Overflow: eviction fires and drops the oldest (e0), positionally — the
        // held Warning is spared because newer entries sit below it, not because
        // the cap is severity-aware.
        _center.Push(new AppNotification(NotifySeverity.Error, "e2"));

        Assert.Equal(3, _center.Items.Count);
        Assert.Contains(reconnect, _center.Items);
        Assert.DoesNotContain(_center.Items, n => string.Equals(n.Message, "e0", StringComparison.Ordinal));

        // The banner is still ordinarily removable afterwards (the close button).
        _center.Remove(reconnect);
        Assert.DoesNotContain(reconnect, _center.Items);
        Assert.Equal(2, _center.Items.Count);
    }
}
