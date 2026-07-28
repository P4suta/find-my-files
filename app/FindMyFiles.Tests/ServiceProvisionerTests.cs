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
        Func<Task<ServiceActionOutcome>>? register = null,
        Func<bool, Task<ServiceActionResult>>? uninstall = null,
        Func<bool>? purgeUserData = null) =>
        new(
            register ?? (() => Task.FromResult(ServiceActionOutcome.Ok)),
            relaunch,
            uninstall,
            purgeUserData);

    [Fact]
    public void RelaunchIntoPipe_triggers_the_relaunch_exactly_once()
    {
        var relaunches = 0;
        var sut = Build(relaunch: () => relaunches++);

        sut.RelaunchIntoPipe();

        Assert.Equal(1, relaunches);
    }

    [Theory]
    [InlineData(0)]
    [InlineData(2)]
    [InlineData(1)]
    public async Task RegisterAsync_returns_the_injected_outcome(int outcomeValue)
    {
        var outcome = (ServiceActionOutcome)outcomeValue;
        var relaunches = 0;
        var sut = Build(relaunch: () => relaunches++, register: () => Task.FromResult(outcome));

        Assert.Equal(outcome, await sut.RegisterAsync());
        Assert.Equal(0, relaunches); // register never relaunches on its own
    }

    [Theory]
    [InlineData(false, "uninstall")]
    [InlineData(true, "uninstall --purge-data")]
    public async Task UninstallAsync_forwards_the_closed_purge_choice(
        bool purgeData,
        string expectedCommand)
    {
        string? command = null;
        var sut = Build(
            relaunch: () => { },
            uninstall: purge =>
            {
                command = purge ? "uninstall --purge-data" : "uninstall";
                return Task.FromResult(
                    new ServiceActionResult(ServiceActionOutcome.Ok, 0));
            });

        var result = await sut.UninstallAsync(purgeData);

        Assert.Equal(ServiceActionOutcome.Ok, result.Service.Outcome);
        Assert.Equal(expectedCommand, command);
    }

    [Theory]
    [InlineData(false, 0)]
    [InlineData(true, 1)]
    public async Task User_data_is_purged_only_after_successful_full_uninstall(
        bool purgeData,
        int expectedPurgeCalls)
    {
        var purgeCalls = 0;
        var sut = Build(
            relaunch: () => { },
            uninstall: _ => Task.FromResult(
                new ServiceActionResult(ServiceActionOutcome.Ok, 0)),
            purgeUserData: () =>
            {
                purgeCalls++;
                return true;
            });

        var result = await sut.UninstallAsync(purgeData);

        Assert.Equal(expectedPurgeCalls, purgeCalls);
        Assert.True(result.UserDataPurged);
    }

    [Fact]
    public async Task Service_failure_never_touches_user_data()
    {
        var purgeCalls = 0;
        var sut = Build(
            relaunch: () => { },
            uninstall: _ => Task.FromResult(
                new ServiceActionResult(ServiceActionOutcome.Failed, 5)),
            purgeUserData: () =>
            {
                purgeCalls++;
                return true;
            });

        var result = await sut.UninstallAsync(purgeData: true);

        Assert.Equal(0, purgeCalls);
        Assert.False(result.UserDataPurged);
    }
}
