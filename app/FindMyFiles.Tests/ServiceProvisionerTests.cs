using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Tests for <see cref="ServiceProvisioner"/>'s two injected boundaries
/// — the elevated register and the pipe-forcing relaunch behind the setup screen
/// and the service manager. Drives both as fakes so the flow runs without a real
/// service, elevation, or exiting the process.</summary>
public sealed class ServiceProvisionerTests
{
    private static ServiceProvisioner Build(
        Action relaunch,
        Func<Task<ServiceActionOutcome>>? register = null) =>
        new(register ?? (() => Task.FromResult(ServiceActionOutcome.Ok)), relaunch);

    [Fact]
    public void RelaunchIntoPipe_triggers_the_relaunch_exactly_once()
    {
        var relaunches = 0;
        var sut = Build(relaunch: () => relaunches++);

        sut.RelaunchIntoPipe();

        Assert.Equal(1, relaunches);
    }

    [Theory]
    [InlineData(ServiceActionOutcome.Ok)]
    [InlineData(ServiceActionOutcome.Cancelled)]
    [InlineData(ServiceActionOutcome.Failed)]
    public async Task RegisterAsync_returns_the_injected_outcome(ServiceActionOutcome outcome)
    {
        var relaunches = 0;
        var sut = Build(relaunch: () => relaunches++, register: () => Task.FromResult(outcome));

        Assert.Equal(outcome, await sut.RegisterAsync());
        Assert.Equal(0, relaunches); // register never relaunches on its own
    }
}
