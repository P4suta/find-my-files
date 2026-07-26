using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Tests for <see cref="EngineClientFactory"/>'s startup transport
/// decision — the auto-mode branch table and command-line helpers. A wrong
/// transport choice disables every feature.</summary>
public sealed class EngineClientFactoryTests
{
    [Fact]
    public void DecideAuto_chooses_pipe_when_running_service_answers()
    {
        var elevCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            serviceState: () => EngineServiceState.Running,
            probe: () => true,
            elevated: () =>
            {
                elevCalls++;
                return true;
            },
            serviceCompatible: () => throw new InvalidOperationException(
                "marker must not be consulted"));

        Assert.Equal(EngineChoice.Pipe, choice);
        Assert.Equal(0, elevCalls);
    }

    [Fact]
    public void DecideAuto_reports_unavailable_when_service_runs_but_rejects_us()
    {
        var elevCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Running,
            () => false,
            () =>
            {
                elevCalls++;
                return true;
            },
            () => throw new InvalidOperationException("marker must not be consulted"));

        Assert.Equal(EngineChoice.UnavailableServiceRejected, choice);
        Assert.Equal(0, elevCalls); // a running service short-circuits before elevation
    }

    [Fact]
    public void DecideAuto_chooses_ffi_when_no_service_and_elevated()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.NotInstalled,
            () => throw new InvalidOperationException("absent service must not be probed"),
            () => true,
            () => throw new InvalidOperationException("marker must not be consulted"));

        Assert.Equal(EngineChoice.Ffi, choice);
    }

    [Fact]
    public void DecideAuto_starts_on_demand_when_service_installed_but_stopped()
    {
        // ADR-0027: only a marker-compatible stopped service starts on demand.
        var elevCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Stopped,
            () => throw new InvalidOperationException("stopped service must not be probed"),
            () =>
            {
                elevCalls++;
                return true;
            },
            () => true);

        Assert.Equal(EngineChoice.StartThenPipe, choice);
        Assert.Equal(0, elevCalls); // a stopped service is started, not bypassed for FFI
    }

    [Fact]
    public void DecideAuto_rejects_stopped_service_with_obsolete_protocol_marker()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Stopped,
            () => throw new InvalidOperationException("stopped service must not be probed"),
            () => throw new InvalidOperationException("elevation must not be consulted"),
            () => false);

        Assert.Equal(EngineChoice.UnavailableServiceIncompatible, choice);
    }

    [Fact]
    public void WithoutService_picks_ffi_or_unavailable()
    {
        Assert.Equal(EngineChoice.Ffi, EngineClientFactory.WithoutService(() => true));
        Assert.Equal(
            EngineChoice.UnavailableNotElevated,
            EngineClientFactory.WithoutService(() => false));
    }

    [Fact]
    public void DecideAuto_reports_unavailable_when_no_service_and_not_elevated()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.NotInstalled,
            () => throw new InvalidOperationException("absent service must not be probed"),
            () => false,
            () => throw new InvalidOperationException("marker must not be consulted"));

        Assert.Equal(EngineChoice.UnavailableNotElevated, choice);
    }

    [Fact]
    public void DecideAuto_unknown_state_fails_closed_to_pipe_only()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Unknown,
            () => false,
            () => throw new InvalidOperationException("elevation must not be consulted"),
            () => throw new InvalidOperationException("marker must not be consulted"));

        Assert.Equal(EngineChoice.UnavailableServiceRejected, choice);
    }

    [Fact]
    public void DecideCustomPipe_probes_without_consulting_scm()
    {
        Assert.Equal(
            EngineChoice.Pipe,
            EngineClientFactory.DecideCustomPipe(() => true, () => false));
        Assert.Equal(
            EngineChoice.UnavailableNotElevated,
            EngineClientFactory.DecideCustomPipe(() => false, () => false));
    }

    [Theory]
    [InlineData(new[] { "--fake-engine" }, true)]
    [InlineData(new[] { "--FAKE-ENGINE" }, true)] // case-insensitive
    [InlineData(new[] { "--engine=pipe" }, false)] // flag absent
    public void HasFlag_matches_case_insensitively(string[] args, bool expected) =>
        Assert.Equal(expected, EngineClientFactory.HasFlag(args, "--fake-engine"));

    [Theory]
    [InlineData(new[] { "--pipe-name=fmf-test" }, "--pipe-name=", "fmf-test")]
    [InlineData(new[] { "--engine=pipe" }, "--engine=", "pipe")]
    [InlineData(new[] { "--other" }, "--engine=", null)]
    public void OptionValue_extracts_the_suffix_or_null(
        string[] args, string prefix, string? expected) =>
        Assert.Equal(expected, EngineClientFactory.OptionValue(args, prefix));

    [Fact]
    public void WithEngineMode_replaces_every_existing_override()
    {
        var args = EngineClientFactory.WithEngineMode(
            ["FindMyFiles.exe", "--engine=pipe", "--other", "--ENGINE=inproc"],
            "unavailable");

        Assert.Equal(["FindMyFiles.exe", "--other", "--engine=unavailable"], args);
    }

    [Fact]
    public void Resolve_unavailable_engine_seam_returns_an_explicit_unavailable_client()
    {
        // `--engine=unavailable` (the UI-automation seam) forces disconnected setup
        // state that `--fake-engine` can't reach (it returns the data-bearing fake).
        using var engine = EngineClientFactory.Resolve(["--engine=unavailable"]);

        Assert.IsType<UnavailableEngineClient>(engine);
        Assert.Equal(EngineClientKind.Unavailable, engine.Kind);
    }

    [Fact]
    public void Resolve_rejects_an_unknown_engine_mode_instead_of_silently_using_auto()
    {
        var error = Assert.Throws<ArgumentException>(
            () => EngineClientFactory.Resolve(["--engine=typo"]));

        Assert.Contains("unsupported --engine mode", error.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void Resolve_rejects_duplicate_or_conflicting_engine_overrides()
    {
        Assert.Throws<ArgumentException>(
            () => EngineClientFactory.Resolve(["--engine=auto", "--ENGINE=pipe"]));
        Assert.Throws<ArgumentException>(
            () => EngineClientFactory.Resolve(["--fake-engine", "--engine=auto"]));
    }
}
