using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural test for <c>TaskExtensions.Forget</c> — the only sanctioned fire-and-forget.
/// A faulted task must surface (log + notification),
/// never be silently dropped (the "don't go silent" rule).</summary>
public sealed class TaskExtensionsTests
{
    [Fact]
    public async Task Forget_routes_a_faulted_task_to_the_notifier()
    {
        const string marker = "forget-marker-7f3a";
        var captured = new TaskCompletionSource<AppNotification>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        using var subscription = Notifier.Attach(n =>
        {
            if (n.Detail?.Contains(marker, StringComparison.Ordinal) == true)
            {
                captured.TrySetResult(n);
            }
        });

        Task.FromException(new InvalidOperationException(marker)).Forget("test-area");

        var notification = await captured.Task.WaitAsync(TimeSpan.FromSeconds(5));
        Assert.Equal(NotifySeverity.Error, notification.Severity);
        Assert.Contains(marker, notification.Detail!, StringComparison.Ordinal);
    }
}
