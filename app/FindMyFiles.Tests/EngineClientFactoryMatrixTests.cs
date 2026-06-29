using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Exhaustive coverage of <see cref="EngineClientFactory"/>'s auto-mode
/// branch table (<c>DecideAuto</c>) and the no-service helper
/// (<c>WithoutService</c>) — every cell of the probe × state × elevation × scope
/// matrix, plus the short-circuits proven by making the un-consulted probes throw.
/// The existing suite covers the headline branches; this fills the simplest
/// state-transition cells (defense in depth — those are the ones that slip).</summary>
public sealed class EngineClientFactoryMatrixTests
{
    private static bool Boom() => throw new InvalidOperationException("delegate must not be consulted");

    private static EngineServiceState BoomState() =>
        throw new InvalidOperationException("delegate must not be consulted");

    [Fact]
    public void Probe_success_short_circuits_without_touching_state_elevation_or_scope()
    {
        // A successful probe is the only input that matters: the remaining three
        // delegates throw, so reaching them would fail the test, not silently pass.
        var choice = EngineClientFactory.DecideAuto(
            probe: () => true,
            serviceState: BoomState,
            elevated: Boom,
            hasScopeConfig: Boom);

        Assert.Equal(EngineChoice.Pipe, choice);
    }

    [Fact]
    public void Probe_failure_with_running_service_is_unreachable_without_consulting_elevation_or_scope()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => false,
            () => EngineServiceState.Running,
            Boom,
            Boom);

        Assert.Equal(EngineChoice.EmptyServiceUnreachable, choice);
    }

    [Fact]
    public void Probe_failure_with_stopped_service_starts_on_demand_without_consulting_elevation_or_scope()
    {
        // ADR-0027: an installed-but-stopped service is started on demand
        // regardless of elevation or scope — neither probe is consulted, so
        // making them throw proves the routing is independent of those inputs.
        var choice = EngineClientFactory.DecideAuto(
            () => false,
            () => EngineServiceState.Stopped,
            Boom,
            Boom);

        Assert.Equal(EngineChoice.StartThenPipe, choice);
    }

    [Fact]
    public void Probe_failure_with_absent_service_and_elevated_is_ffi_without_consulting_scope()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => false,
            () => EngineServiceState.NotInstalled,
            () => true,
            Boom); // elevation wins; scope must not be consulted

        Assert.Equal(EngineChoice.Ffi, choice);
    }

    [Fact]
    public void Probe_failure_with_absent_service_not_elevated_and_scope_configured_is_walk_in_proc()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => false,
            () => EngineServiceState.NotInstalled,
            () => false,
            () => true);

        Assert.Equal(EngineChoice.WalkInProc, choice);
    }

    [Fact]
    public void Probe_failure_with_absent_service_not_elevated_and_no_scope_is_empty_not_elevated()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => false,
            () => EngineServiceState.NotInstalled,
            () => false,
            () => false);

        Assert.Equal(EngineChoice.EmptyNotElevated, choice);
    }

    [Fact]
    public void Without_service_when_elevated_is_ffi_without_consulting_scope()
    {
        // Elevation wins outright — the scope probe must not be reached.
        var choice = EngineClientFactory.WithoutService(() => true, Boom);

        Assert.Equal(EngineChoice.Ffi, choice);
    }

    [Fact]
    public void Without_service_when_not_elevated_with_scope_is_walk_in_proc()
    {
        var choice = EngineClientFactory.WithoutService(() => false, () => true);

        Assert.Equal(EngineChoice.WalkInProc, choice);
    }

    [Fact]
    public void Without_service_when_not_elevated_without_scope_is_empty_not_elevated()
    {
        var choice = EngineClientFactory.WithoutService(() => false, () => false);

        Assert.Equal(EngineChoice.EmptyNotElevated, choice);
    }
}
