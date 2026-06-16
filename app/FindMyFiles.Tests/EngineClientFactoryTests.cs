using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Tests for <see cref="EngineClientFactory"/>'s startup transport
/// decision — the auto-mode branch table (probe → state → elevation) and the
/// command-line helpers. The whole-app behaviour shipped untested; a wrong
/// transport choice disables every feature.</summary>
public sealed class EngineClientFactoryTests
{
    [Fact]
    public void DecideAuto_chooses_pipe_when_the_probe_succeeds()
    {
        var stateCalls = 0;
        var elevCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            probe: () => true,
            serviceState: () => { stateCalls++; return EngineServiceState.Stopped; },
            elevated: () => { elevCalls++; return true; },
            hasScopeConfig: () => false);

        Assert.Equal(EngineChoice.Pipe, choice);
        Assert.Equal(0, stateCalls); // a successful probe short-circuits
        Assert.Equal(0, elevCalls);
    }

    [Fact]
    public void DecideAuto_chooses_empty_when_service_runs_but_rejects_us()
    {
        var elevCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            () => false, () => EngineServiceState.Running, () => { elevCalls++; return true; },
            () => false);

        Assert.Equal(EngineChoice.EmptyServiceUnreachable, choice);
        Assert.Equal(0, elevCalls); // a running service short-circuits before elevation
    }

    [Fact]
    public void DecideAuto_chooses_ffi_when_no_service_and_elevated()
    {
        var scopeCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            () => false, () => EngineServiceState.Stopped, () => true,
            () => { scopeCalls++; return true; });

        Assert.Equal(EngineChoice.Ffi, choice);
        Assert.Equal(0, scopeCalls); // elevation short-circuits before scope config
    }

    [Fact]
    public void DecideAuto_chooses_empty_when_no_service_not_elevated_and_no_scope()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => false, () => EngineServiceState.NotInstalled, () => false, () => false);

        Assert.Equal(EngineChoice.EmptyNotElevated, choice);
    }

    [Fact]
    public void DecideAuto_chooses_walk_when_no_service_not_elevated_and_scope_configured()
    {
        // ADR-0024: the corporate-PC path — admin forbidden, but the user picked
        // folders to fall back on.
        var choice = EngineClientFactory.DecideAuto(
            () => false, () => EngineServiceState.NotInstalled, () => false, () => true);

        Assert.Equal(EngineChoice.WalkInProc, choice);
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
}
