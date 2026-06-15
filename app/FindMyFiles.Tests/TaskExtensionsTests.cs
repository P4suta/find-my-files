using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Behavioural test for <c>TaskExtensions.Forget</c> — the only
/// sanctioned fire-and-forget. A faulted task must surface (log + notification),
/// never be silently dropped (the "黙らない" rule).</summary>
public sealed class TaskExtensionsTests
{
    [Fact]
    public async Task Forget_routes_a_faulted_task_to_the_notifier()
    {
        // Notifier is a process-wide static funnel shared by every test, so a
        // unique marker ensures only THIS test's notification completes the wait.
        const string marker = "forget-marker-7f3a";
        var captured = new TaskCompletionSource<AppNotification>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        Notifier.Attach(n =>
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
