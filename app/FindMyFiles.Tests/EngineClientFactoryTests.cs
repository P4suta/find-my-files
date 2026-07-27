using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.ViewModels;
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
        var choice = EngineClientFactory.DecideAuto(
            serviceState: () => EngineServiceState.Running,
            probe: () => true,
            serviceCompatible: () => throw new InvalidOperationException(
                "marker must not be consulted"));

        Assert.Equal(EngineChoice.Pipe, choice);
    }

    [Fact]
    public void DecideAuto_reports_unavailable_when_service_runs_but_rejects_us()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Running,
            () => false,
            () => throw new InvalidOperationException("marker must not be consulted"));

        Assert.Equal(EngineChoice.UnavailableServiceRejected, choice);
    }

    [Fact]
    public void DecideAuto_never_falls_back_to_in_proc_when_no_service_is_installed()
    {
        // Security: an elevated launch used to auto-select the in-proc engine,
        // which creates %ProgramData%\find-my-files itself — without the hardened
        // descriptor service install applies — leaving the machine index (every
        // file name on the box, docs/SECURITY.md threat 7) under C:\ProgramData's
        // inherited "Users: read + write" ACL. The absent service must instead
        // route to the setup screen, whatever token this process holds.
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.NotInstalled,
            () => throw new InvalidOperationException("absent service must not be probed"),
            () => throw new InvalidOperationException("marker must not be consulted"));

        Assert.Equal(EngineChoice.UnavailableNoService, choice);
    }

    [Fact]
    public void EngineChoice_offers_no_in_proc_outcome_at_all()
    {
        // Structural companion to the test above: elevation is not even an input
        // to the auto decision, so the only way to re-introduce the ProgramData
        // ACL hole is to re-introduce an in-proc member here.
        Assert.All(
            Enum.GetNames<EngineChoice>(),
            name =>
            {
                Assert.DoesNotContain("Ffi", name, StringComparison.OrdinalIgnoreCase);
                Assert.DoesNotContain("InProc", name, StringComparison.OrdinalIgnoreCase);
            });
    }

    [Fact]
    public void DecideAuto_resolves_every_scm_state_to_a_pipe_or_unavailable_outcome()
    {
        // Whole-domain sweep (every SCM state × probe × marker): auto mode has no
        // in-proc escape hatch from any starting point, so no combination can put
        // the machine index outside the service-hardened data root.
        var allowed = new[]
        {
            EngineChoice.Pipe,
            EngineChoice.StartThenPipe,
            EngineChoice.UnavailableServiceRejected,
            EngineChoice.UnavailableServiceIncompatible,
            EngineChoice.UnavailableNoService,
        };

        foreach (var state in Enum.GetValues<EngineServiceState>())
        {
            foreach (var probe in new[] { true, false })
            {
                foreach (var compatible in new[] { true, false })
                {
                    var choice = EngineClientFactory.DecideAuto(
                        () => state,
                        () => probe,
                        () => compatible);

                    Assert.Contains(choice, allowed);
                }
            }
        }
    }

    [Fact]
    public void DecideAuto_starts_on_demand_when_service_installed_but_stopped()
    {
        // ADR-0027: only a marker-compatible stopped service starts on demand.
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Stopped,
            () => throw new InvalidOperationException("stopped service must not be probed"),
            () => true);

        Assert.Equal(EngineChoice.StartThenPipe, choice);
    }

    [Fact]
    public void DecideAuto_rejects_stopped_service_with_obsolete_protocol_marker()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Stopped,
            () => throw new InvalidOperationException("stopped service must not be probed"),
            () => false);

        Assert.Equal(EngineChoice.UnavailableServiceIncompatible, choice);
    }

    [Fact]
    public void DecideAuto_unknown_state_fails_closed_to_pipe_only()
    {
        var choice = EngineClientFactory.DecideAuto(
            () => EngineServiceState.Unknown,
            () => false,
            () => throw new InvalidOperationException("marker must not be consulted"));

        Assert.Equal(EngineChoice.UnavailableServiceRejected, choice);
    }

    [Fact]
    public void DecideCustomPipe_probes_without_consulting_scm()
    {
        Assert.Equal(
            EngineChoice.Pipe,
            EngineClientFactory.DecideCustomPipe(() => true));
        Assert.Equal(
            EngineChoice.UnavailableNoService,
            EngineClientFactory.DecideCustomPipe(() => false));
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
    public async Task Resolve_unavailable_engine_seam_returns_an_explicit_unavailable_client()
    {
        // `--engine=unavailable` (the UI-automation seam) forces disconnected setup
        // state that `--fake-engine` can't reach (it returns the data-bearing fake).
        using var engine = EngineClientFactory.Resolve(["--engine=unavailable"]);

        Assert.IsType<UnavailableEngineClient>(engine);
        Assert.Equal(EngineClientKind.Unavailable, engine.Kind);
        Assert.Equal(EngineConnectionState.Unavailable, engine.Connection);
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => engine.ListVolumesAsync());
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => engine.StartIndexingAsync(["C:"]));
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => engine.GetStatusAsync());
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => engine.SearchAsync("readme", SearchOptions.Default));
        Assert.Null(await engine.GetStatsAsync());
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

    [Fact]
    public async Task ResolveAsync_returns_before_a_blocking_resolver_completes()
    {
        using var release = new ManualResetEventSlim();
        var entered = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);

        var resolving = EngineClientFactory.ResolveAsync(() =>
        {
            entered.TrySetResult();
            Assert.True(release.Wait(TimeSpan.FromSeconds(5)));
            return new UnavailableEngineClient();
        });

        await entered.Task.WaitAsync(TimeSpan.FromSeconds(5));
        Assert.False(resolving.IsCompleted);
        release.Set();

        using var engine = await resolving;
        Assert.IsType<UnavailableEngineClient>(engine);
    }

    [Fact]
    public async Task ResolvingPlaceholder_is_connecting_and_contains_no_data()
    {
        using var engine = new ResolvingEngineClient();

        Assert.Equal(EngineClientKind.Resolving, engine.Kind);
        Assert.Equal(EngineConnectionState.Connecting, engine.Connection);
        Assert.Equal(Loc.Get("EngineMode_Connecting"), StatusFormatter.EngineMode(engine));
        await Assert.ThrowsAsync<EngineUnavailableException>(
            () => engine.SearchAsync("readme", SearchOptions.Default));
        Assert.Null(await engine.GetStatsAsync());
    }
}
