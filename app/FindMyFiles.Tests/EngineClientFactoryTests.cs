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
            serviceState: () =>
            {
                stateCalls++;
                return EngineServiceState.Stopped;
            },
            elevated: () =>
            {
                elevCalls++;
                return true;
            },
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
            () => false,
            () => EngineServiceState.Running,
            () =>
            {
                elevCalls++;
                return true;
            },
            () => false);

        Assert.Equal(EngineChoice.EmptyServiceUnreachable, choice);
        Assert.Equal(0, elevCalls); // a running service short-circuits before elevation
    }

    [Fact]
    public void DecideAuto_chooses_ffi_when_no_service_and_elevated()
    {
        var scopeCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            () => false,
            () => EngineServiceState.NotInstalled,
            () => true,
            () =>
            {
                scopeCalls++;
                return true;
            });

        Assert.Equal(EngineChoice.Ffi, choice);
        Assert.Equal(0, scopeCalls); // elevation short-circuits before scope config
    }

    [Fact]
    public void DecideAuto_starts_on_demand_when_service_installed_but_stopped()
    {
        // ADR-0027: an installed-but-stopped service is started on demand,
        // regardless of elevation. Resolve owns the start + the fall-back-on-
        // failure path; DecideAuto only routes to StartThenPipe.
        var elevCalls = 0;

        var choice = EngineClientFactory.DecideAuto(
            () => false,
            () => EngineServiceState.Stopped,
            () =>
            {
                elevCalls++;
                return true;
            },
            () => false);

        Assert.Equal(EngineChoice.StartThenPipe, choice);
        Assert.Equal(0, elevCalls); // a stopped service is started, not bypassed for FFI
    }

    [Fact]
    public void WithoutService_picks_ffi_then_scope_then_empty()
    {
        // Elevated → FFI (even with scope configured: elevation wins).
        Assert.Equal(EngineChoice.Ffi, EngineClientFactory.WithoutService(() => true, () => false));
        Assert.Equal(EngineChoice.Ffi, EngineClientFactory.WithoutService(() => true, () => true));

        // Not elevated → scope walk when roots exist, else the empty/setup engine.
        Assert.Equal(
            EngineChoice.WalkInProc, EngineClientFactory.WithoutService(() => false, () => true));
        Assert.Equal(
            EngineChoice.EmptyNotElevated,
            EngineClientFactory.WithoutService(() => false, () => false));
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

    [Fact]
    public void Resolve_empty_engine_seam_returns_the_disconnected_fake()
    {
        // `--engine=empty` (the UI-automation seam) forces the disconnected setup
        // state that `--fake-engine` can't reach (it returns the data-bearing fake).
        var engine = EngineClientFactory.Resolve(["--engine=empty"]);

        Assert.True(engine is FakeEngineClient { IsEmpty: true });
    }
}
