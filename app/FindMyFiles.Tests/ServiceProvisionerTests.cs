using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Tests for <see cref="ServiceProvisioner"/>'s two injected boundaries
/// — the elevated register and the pipe-forcing relaunch behind the setup screen
/// and the service manager. Drives both as fakes so the flow runs without a real
/// service, elevation, or exiting the process.</summary>
public sealed class ServiceProvisionerTests
{
    private sealed class ProductionHarness
    {
        public string? Exe { get; set; } = @"C:\bundle\fmf-service.exe";

        public bool SetupArgumentsAvailable { get; set; } = true;

        public ServiceActionResult Result { get; set; } =
            new(ServiceActionOutcome.Ok, 0);

        public List<(string Exe, string Arguments)> Runs { get; } = [];

        public ServiceProvisionerHooks Hooks => new(
            _ => Exe,
            () => (SetupArgumentsAvailable, "setup --owner-sid=S-1-5-21-1"),
            (exe, arguments) =>
            {
                Runs.Add((exe, arguments));
                return Result;
            });
    }

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

    [Fact]
    public async Task Real_register_fails_closed_when_the_helper_is_missing()
    {
        var harness = new ProductionHarness { Exe = null };
        using var hooks = ServiceProvisioner.UseHooksForTests(harness.Hooks);

        Assert.Equal(ServiceActionOutcome.Failed, await ServiceProvisioner.Real.RegisterAsync());
        Assert.Empty(harness.Runs);
    }

    [Fact]
    public async Task Real_register_refuses_an_ownerless_setup()
    {
        var harness = new ProductionHarness { SetupArgumentsAvailable = false };
        using var hooks = ServiceProvisioner.UseHooksForTests(harness.Hooks);

        Assert.Equal(
            ServiceActionOutcome.IdentityUnavailable,
            await ServiceProvisioner.Real.RegisterAsync());
        Assert.Empty(harness.Runs);
    }

    [Theory]
    [InlineData(0)]
    [InlineData(1)]
    [InlineData(2)]
    public async Task Real_register_forwards_the_exact_closed_setup_command(int outcomeValue)
    {
        var expected = (ServiceActionOutcome)outcomeValue;
        var harness = new ProductionHarness
        {
            Result = new ServiceActionResult(expected, 17),
        };
        using var hooks = ServiceProvisioner.UseHooksForTests(harness.Hooks);

        Assert.Equal(expected, await ServiceProvisioner.Real.RegisterAsync());
        Assert.Equal(
            [(@"C:\bundle\fmf-service.exe", "setup --owner-sid=S-1-5-21-1")],
            harness.Runs);
    }

    [Fact]
    public async Task Real_uninstall_fails_closed_when_the_helper_is_missing()
    {
        var harness = new ProductionHarness { Exe = null };
        using var hooks = ServiceProvisioner.UseHooksForTests(harness.Hooks);

        var result = await ServiceProvisioner.UninstallElevatedAsync(purgeData: false);

        Assert.Equal(ServiceActionOutcome.Failed, result.Outcome);
        Assert.Equal(-1, result.ExitCode);
        Assert.Empty(harness.Runs);
    }

    [Theory]
    [InlineData(false, "uninstall")]
    [InlineData(true, "uninstall --purge-data")]
    public async Task Real_uninstall_forwards_only_the_closed_purge_command(
        bool purgeData,
        string expectedArguments)
    {
        var harness = new ProductionHarness();
        using var hooks = ServiceProvisioner.UseHooksForTests(harness.Hooks);
        var result = await ServiceProvisioner.UninstallElevatedAsync(purgeData);

        Assert.Equal(ServiceActionOutcome.Ok, result.Outcome);
        Assert.Equal(
            [(@"C:\bundle\fmf-service.exe", expectedArguments)],
            harness.Runs);
    }

    [Fact]
    public void Production_hook_scope_rejects_null()
    {
        Assert.Throws<ArgumentNullException>(
            () => ServiceProvisioner.UseHooksForTests(null!));
    }

    [Fact]
    public async Task Production_paths_emit_complete_structured_diagnostics()
    {
        using var log = new LogCapture();
        var missing = new ProductionHarness { Exe = null };
        using (ServiceProvisioner.UseHooksForTests(missing.Hooks))
        {
            _ = await ServiceProvisioner.Real.RegisterAsync();
            _ = await ServiceProvisioner.UninstallElevatedAsync(purgeData: false);
        }

        var ownerless = new ProductionHarness { SetupArgumentsAvailable = false };
        using (ServiceProvisioner.UseHooksForTests(ownerless.Hooks))
        {
            _ = await ServiceProvisioner.Real.RegisterAsync();
        }

        var completed = new ProductionHarness
        {
            Result = new ServiceActionResult(ServiceActionOutcome.Failed, 17),
        };
        using (ServiceProvisioner.UseHooksForTests(completed.Hooks))
        {
            _ = await ServiceProvisioner.Real.RegisterAsync();
            _ = await ServiceProvisioner.UninstallElevatedAsync(purgeData: true);
        }

        var lines = log.Text.Split('\n');
        AssertLog(lines, "area=service-ui", "fmf-service.exe not found — cannot register");
        AssertLog(lines, "area=service-ui", "fmf-service.exe not found — cannot uninstall");
        AssertLog(
            lines,
            "area=service-ui",
            "current user SID unavailable or invalid — refusing owner-less elevated setup");
        AssertLog(
            lines,
            "area=service-ui",
            "outcome=1",
            "exit=17",
            "service action completed");
        Assert.Contains(
            lines,
            line => line.Contains("area=service-ui", StringComparison.Ordinal)
                && line.Contains("outcome=1", StringComparison.Ordinal)
                && line.Contains("exit=17", StringComparison.Ordinal)
                && line.Contains("service action completed", StringComparison.Ordinal)
                && !line.Contains("verb=", StringComparison.Ordinal));
        AssertLog(
            lines,
            "area=service-ui",
            "verb=uninstall",
            "purge_data=true",
            "outcome=1",
            "exit=17",
            "service action completed");
    }

    private static void AssertLog(string[] lines, params string[] fragments) =>
        Assert.Contains(
            lines,
            line => fragments.All(fragment => line.Contains(fragment, StringComparison.Ordinal)));
}
