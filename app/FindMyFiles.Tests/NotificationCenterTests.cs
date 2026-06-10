using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class NotificationCenterTests
{
    private readonly ManualDispatcher _dispatcher = new();
    private readonly NotificationCenter _center;

    public NotificationCenterTests()
    {
        _center = new NotificationCenter(_dispatcher);
    }

    [Fact]
    public void Push_BeyondTheCap_DropsTheOldestFirst()
    {
        for (var i = 0; i < 4; i++)
        {
            _center.Push(new AppNotification(NotifySeverity.Error, $"n{i}"));
        }

        Assert.Equal(3, _center.Items.Count);
        Assert.Equal(["n1", "n2", "n3"], _center.Items.Select(n => n.Message));
    }

    [Fact]
    public void Push_Info_AutoDismissesWhenItsTimerFires()
    {
        _center.Push(new AppNotification(NotifySeverity.Info, "saved"));

        Assert.Single(_center.Items);
        var timer = Assert.Single(_dispatcher.Timers);
        Assert.True(timer.IsStarted);
        Assert.Equal(TimeSpan.FromSeconds(5), timer.Interval);

        timer.Fire();

        Assert.Empty(_center.Items);
    }

    [Fact]
    public void Push_ErrorAndWarning_NeverAutoDismiss()
    {
        _center.Push(new AppNotification(NotifySeverity.Error, "boom"));
        _center.Push(new AppNotification(NotifySeverity.Warning, "hmm"));

        Assert.Empty(_dispatcher.Timers); // no dissolve timer was even created
        _dispatcher.FireTimers();
        Assert.Equal(2, _center.Items.Count);
    }

    [Fact]
    public void Remove_TakesTheNotificationOut()
    {
        var n = new AppNotification(NotifySeverity.Error, "boom");
        _center.Push(n);

        _center.Remove(n);

        Assert.Empty(_center.Items);
    }
}
