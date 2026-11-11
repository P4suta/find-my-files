using Microsoft.Extensions.Time.Testing;

namespace FindMyFiles.Tests.TestDoubles;

internal sealed class ObservedTimeProvider : FakeTimeProvider
{
    internal Action? TimerCreated { get; set; }

    public override ITimer CreateTimer(
        TimerCallback callback,
        object? state,
        TimeSpan dueTime,
        TimeSpan period)
    {
        var timer = base.CreateTimer(callback, state, dueTime, period);
        TimerCreated?.Invoke();
        return timer;
    }
}
