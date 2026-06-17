using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Tests for <see cref="ServiceProvisioner"/>'s post-register loop —
/// the wait-for-pipe-then-relaunch step behind the setup screen and the service
/// manager. Drives the injectable boundaries (probe / relaunch / delay) so the
/// flow runs without a real service, elevation, or exiting the process. The
/// relaunch count is the load-bearing assertion: relaunch exactly once on
/// success, never when the service stays silent.</summary>
public sealed class ServiceProvisionerTests
{
    private static ServiceProvisioner Build(
        Func<string, TimeSpan, Task<bool>> probe,
        Action relaunch,
        Func<Task<ServiceActionOutcome>>? register = null,
        Func<TimeSpan, Task>? delay = null) =>
        new(
            register ?? (() => Task.FromResult(ServiceActionOutcome.Ok)),
            probe,
            relaunch,
            delay ?? (_ => Task.CompletedTask));

    [Fact]
    public async Task WaitForServiceThenRelaunch_relaunches_once_when_the_pipe_answers_immediately()
    {
        var relaunches = 0;
        var probes = 0;
        var sut = Build(
            probe: (_, _) =>
            {
                probes++;
                return Task.FromResult(true);
            },
            relaunch: () => relaunches++);

        var ok = await sut.WaitForServiceThenRelaunchAsync();

        Assert.True(ok);
        Assert.Equal(1, relaunches);
        Assert.Equal(1, probes); // returns on the first successful probe — no extra polling
    }

    [Fact]
    public async Task WaitForServiceThenRelaunch_relaunches_after_the_pipe_comes_up_late()
    {
        var relaunches = 0;
        var probes = 0;
        var sut = Build(
            probe: (_, _) =>
            {
                probes++;
                return Task.FromResult(probes >= 3); // false, false, then true
            },
            relaunch: () => relaunches++);

        var ok = await sut.WaitForServiceThenRelaunchAsync();

        Assert.True(ok);
        Assert.Equal(1, relaunches);
        Assert.Equal(3, probes);
    }

    [Fact]
    public async Task WaitForServiceThenRelaunch_gives_up_without_relaunching_when_the_pipe_stays_silent()
    {
        var relaunches = 0;
        var probes = 0;
        var delays = 0;
        var sut = Build(
            probe: (_, _) =>
            {
                probes++;
                return Task.FromResult(false);
            },
            relaunch: () => relaunches++,
            delay: _ =>
            {
                delays++;
                return Task.CompletedTask;
            });

        var ok = await sut.WaitForServiceThenRelaunchAsync();

        Assert.False(ok);
        Assert.Equal(0, relaunches); // never relaunch into a service that didn't come up
        Assert.Equal(16, probes);    // the full ≈8s budget (16 attempts)
        Assert.Equal(16, delays);
    }

    [Fact]
    public async Task RegisterAsync_returns_the_injected_outcome()
    {
        var sut = Build(
            probe: (_, _) => Task.FromResult(true),
            relaunch: () => { },
            register: () => Task.FromResult(ServiceActionOutcome.Cancelled));

        Assert.Equal(ServiceActionOutcome.Cancelled, await sut.RegisterAsync());
    }
}
