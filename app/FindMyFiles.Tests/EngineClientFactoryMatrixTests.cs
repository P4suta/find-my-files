using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Exhaustive coverage of <see cref="EngineClientFactory"/>'s auto-mode
/// branch table (<c>DecideAuto</c>) and the no-service helper
/// (<c>WithoutService</c>) — every cell of the probe × state × marker × elevation
/// matrix, including short-circuits.</summary>
public sealed class EngineClientFactoryMatrixTests
{
    private static bool Boom() => throw new InvalidOperationException("delegate must not be consulted");

    [Fact]
    public void Running_service_with_successful_probe_short_circuits_elevation()
    {
        var choice = EngineClientFactory.DecideAuto(
            serviceState: () => EngineServiceState.Running,
            probe: () => true,
            elevated: Boom,
            serviceCompatible: Boom);

        Assert.Equal(EngineChoice.Pipe, choice);
    }

    [Fact]
    public void Probe_failure_with_running_service_is_unreachable_without_consulting_elevation()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Running,
            () => false,
            Boom,
            Boom);

        Assert.Equal(EngineChoice.UnavailableServiceRejected, choice);
    }

    [Fact]
    public void Probe_failure_with_stopped_service_starts_on_demand_without_consulting_elevation()
    {
        // ADR-0027: a marker-compatible stopped service starts on demand.
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Stopped,
            Boom,
            Boom,
            () => true);

        Assert.Equal(EngineChoice.StartThenPipe, choice);
    }

    [Fact]
    public void Probe_failure_with_obsolete_stopped_service_requires_setup()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Stopped,
            Boom,
            Boom,
            () => false);

        Assert.Equal(EngineChoice.UnavailableServiceIncompatible, choice);
    }

    [Fact]
    public void Probe_failure_with_absent_service_and_elevated_is_ffi()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.NotInstalled,
            Boom,
            () => true,
            Boom); // no-service flow must not consult the marker

        Assert.Equal(EngineChoice.Ffi, choice);
    }

    [Fact]
    public void Probe_failure_with_absent_service_not_elevated_is_unavailable()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.NotInstalled,
            Boom,
            () => false,
            Boom);

        Assert.Equal(EngineChoice.UnavailableNotElevated, choice);
    }

    [Theory]
    [InlineData(false, true)]
    [InlineData(false, false)]
    [InlineData(true, true)]
    [InlineData(true, false)]
    public void Live_or_unknown_state_is_pipe_only(bool unknown, bool probe)
    {
        var state = unknown ? EngineServiceState.Unknown : EngineServiceState.Running;
        var choice = EngineClientFactory.DecideAuto(
            () => state,
            () => probe,
            Boom,
            Boom);

        Assert.Equal(
            probe ? EngineChoice.Pipe : EngineChoice.UnavailableServiceRejected,
            choice);
    }

    [Fact]
    public void Without_service_when_elevated_is_ffi()
    {
        var choice = EngineClientFactory.WithoutService(() => true);

        Assert.Equal(EngineChoice.Ffi, choice);
    }

    [Fact]
    public void Without_service_when_not_elevated_is_unavailable()
    {
        var choice = EngineClientFactory.WithoutService(() => false);

        Assert.Equal(EngineChoice.UnavailableNotElevated, choice);
    }
}
