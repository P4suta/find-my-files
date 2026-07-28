using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class AsyncSingleFlightTests
{
    [Fact]
    public async Task Overlapping_refreshes_are_coalesced()
    {
        using var gate = new AsyncSingleFlight();
        var release = new TaskCompletionSource<bool>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var calls = 0;

        var first = gate.RunAsync(async () =>
        {
            calls++;
            await release.Task;
        });
        var overlapping = gate.RunAsync(() =>
        {
            calls++;
            return Task.CompletedTask;
        });

        await overlapping;
        Assert.Equal(1, calls);
        release.SetResult(true);
        await first;

        await gate.RunAsync(() =>
        {
            calls++;
            return Task.CompletedTask;
        });
        Assert.Equal(2, calls);
    }

    [Fact]
    public async Task Dispose_rejects_future_refreshes()
    {
        var gate = new AsyncSingleFlight();
        gate.Dispose();
        var calls = 0;

        await gate.RunAsync(() =>
        {
            calls++;
            return Task.CompletedTask;
        });

        Assert.Equal(0, calls);
    }
}
