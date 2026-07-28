using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Exhaustive coverage of <see cref="EngineClientFactory"/>'s auto-mode
/// branch table (<c>DecideAuto</c>) — every cell of the probe × state × marker
/// matrix, including short-circuits. Elevation is intentionally absent from the
/// matrix: auto mode must not consider it (see
/// <see cref="EngineClientFactoryTests.EngineChoice_offers_no_in_proc_outcome_at_all"/>).</summary>
public sealed class EngineClientFactoryMatrixTests
{
    private static bool Boom() => throw new InvalidOperationException("delegate must not be consulted");

    [Fact]
    public void Running_service_with_successful_probe_short_circuits_the_marker()
    {
        var choice = EngineClientFactory.DecideAuto(
            serviceState: () => EngineServiceState.Running,
            probe: () => true,
            serviceCompatible: Boom);

        Assert.Equal(EngineChoice.Pipe, choice);
    }

    [Fact]
    public void Probe_failure_with_running_service_is_unreachable_without_consulting_the_marker()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Running,
            () => false,
            Boom);

        Assert.Equal(EngineChoice.UnavailableServiceRejected, choice);
    }

    [Fact]
    public void Stopped_service_starts_on_demand_without_being_probed()
    {
        // ADR-0027: a marker-compatible stopped service starts on demand.
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Stopped,
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
            () => false);

        Assert.Equal(EngineChoice.UnavailableServiceIncompatible, choice);
    }

    [Fact]
    public void Absent_service_requires_setup_and_consults_nothing_else()
    {
        // The absent-service cell has exactly one outcome — the setup screen.
        // Both remaining inputs are Boom, so any re-introduced token/probe test
        // on this path (the old elevated → in-proc fallback) fails here.
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.NotInstalled,
            Boom,
            Boom);

        Assert.Equal(EngineChoice.UnavailableNoService, choice);
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
            Boom);

        Assert.Equal(
            probe ? EngineChoice.Pipe : EngineChoice.UnavailableServiceRejected,
            choice);
    }
}
