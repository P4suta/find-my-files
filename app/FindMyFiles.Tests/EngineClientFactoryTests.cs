using FindMyFiles.Engine;
using FindMyFiles.Services;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Serilog;
using Serilog.Core;
using Serilog.Events;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Tests for <see cref="EngineClientFactory"/>'s startup transport
/// decision — the auto-mode branch table and command-line helpers. A wrong
/// transport choice disables every feature.</summary>
public sealed class EngineClientFactoryTests
{
    private sealed class Harness
    {
        public EngineServiceState State { get; set; } = EngineServiceState.NotInstalled;

        public bool ProbeResult { get; set; }

        public bool Compatible { get; set; }

        public bool StartResult { get; set; }

        public bool Elevated { get; set; }

        public int ProbeCalls { get; private set; }

        public int QueryStateCalls { get; private set; }

        public string? ProbedPipeName { get; private set; }

        public string? OpenedPipeName { get; private set; }

        public StubEngineClient PipeClient { get; } = new() { Kind = EngineClientKind.Service };

        public StubEngineClient InProcClient { get; } = new() { Kind = EngineClientKind.InProcess };

        public EngineClientFactoryHooks Hooks => new(
            pipeName =>
            {
                ProbeCalls++;
                ProbedPipeName = pipeName;
                return ProbeResult;
            },
            () =>
            {
                QueryStateCalls++;
                return State;
            },
            () => Compatible,
            () => StartResult,
            () => Elevated,
            pipeName =>
            {
                OpenedPipeName = pipeName;
                return PipeClient;
            },
            () => InProcClient);
    }

    private static (T Result, string Log) CaptureLog<T>(Func<T> action)
    {
        var previous = Log.Logger;
        var sink = new CaptureSink();
        using var logger = new LoggerConfiguration()
            .MinimumLevel.Debug()
            .Enrich.FromLogContext()
            .WriteTo.Sink(sink)
            .CreateLogger();
        try
        {
            Log.Logger = logger;
            return (action(), sink.Text);
        }
        finally
        {
            Log.Logger = previous;
        }
    }

    private sealed class CaptureSink : ILogEventSink
    {
        private readonly LogfmtFormatter _formatter = new();
        private readonly StringWriter _writer = new();

        public string Text => _writer.ToString();

        public void Emit(LogEvent logEvent) => _formatter.Format(logEvent, _writer);
    }

    private static (IEngineClient Engine, string Log) ResolveAndCapture(
        string[] args,
        Harness harness)
    {
        return CaptureLog(() => EngineClientFactory.Resolve(args, harness.Hooks));
    }

    private static void AssertLogMessage(string contents, string message)
    {
        Assert.Contains(
            contents.Split('\n'),
            line => line.Contains("area=app", StringComparison.Ordinal)
                && line.EndsWith($"msg=\"{message}\"", StringComparison.Ordinal));
    }

    [Fact]
    public void Resolve_with_injected_boundaries_covers_every_transport_outcome()
    {
        var fakeHarness = new Harness();
        var (fake, fakeLog) = ResolveAndCapture(["--fake-engine"], fakeHarness);
        using (fake)
        {
            Assert.IsType<FakeEngineClient>(fake);
        }

        AssertLogMessage(fakeLog, "data root selected");
        Assert.Contains("test_override=", fakeLog, StringComparison.Ordinal);
        AssertLogMessage(fakeLog, "engine: fake (--fake-engine)");

        var unavailableHarness = new Harness();
        var (forcedUnavailable, forcedUnavailableLog) = ResolveAndCapture(
            ["--engine=unavailable"], unavailableHarness);
        Assert.IsType<UnavailableEngineClient>(forcedUnavailable);
        AssertLogMessage(
            forcedUnavailableLog,
            "engine: unavailable (--engine=unavailable test seam)");

        var explicitPipeHarness = new Harness();
        var (explicitPipe, explicitPipeLog) = ResolveAndCapture(
            ["--engine=pipe"], explicitPipeHarness);
        Assert.Same(explicitPipeHarness.PipeClient, explicitPipe);
        Assert.Equal(PipeProtocol.DefaultPipeName, explicitPipeHarness.OpenedPipeName);
        AssertLogMessage(explicitPipeLog, "engine: pipe (explicit)");

        var inProcHarness = new Harness { State = EngineServiceState.NotInstalled };
        var (inProc, inProcLog) = ResolveAndCapture(["--engine=inproc"], inProcHarness);
        Assert.Same(inProcHarness.InProcClient, inProc);
        Assert.Equal(1, inProcHarness.QueryStateCalls);
        Assert.Contains("service_installed=false", inProcLog, StringComparison.Ordinal);
        AssertLogMessage(
            inProcLog,
            "engine: in-proc FFI (explicit --engine=inproc) — the machine index is outside the service-hardened data root");

        var customPipeHarness = new Harness { ProbeResult = true };
        var (customPipe, customPipeLog) = ResolveAndCapture(
            ["--pipe-name=fmf-custom"], customPipeHarness);
        Assert.Same(customPipeHarness.PipeClient, customPipe);
        Assert.Equal("fmf-custom", customPipeHarness.ProbedPipeName);
        Assert.Equal("fmf-custom", customPipeHarness.OpenedPipeName);
        Assert.Equal(0, customPipeHarness.QueryStateCalls);
        AssertLogMessage(customPipeLog, "engine: pipe (probe succeeded)");

        var customMissingHarness = new Harness { Elevated = true };
        var (customMissing, customMissingLog) = ResolveAndCapture(
            ["--pipe-name=fmf-missing"], customMissingHarness);
        Assert.IsType<UnavailableEngineClient>(customMissing);
        Assert.Equal("fmf-missing", customMissingHarness.ProbedPipeName);
        Assert.Equal(0, customMissingHarness.QueryStateCalls);
        Assert.Contains("elevated=true", customMissingLog, StringComparison.Ordinal);
        AssertLogMessage(
            customMissingLog,
            "engine: unavailable (no service registered) — setup required");

        var runningHarness = new Harness
        {
            State = EngineServiceState.Running,
            ProbeResult = true,
        };
        var (running, runningLog) = ResolveAndCapture([], runningHarness);
        Assert.Same(runningHarness.PipeClient, running);
        Assert.Equal(PipeProtocol.DefaultPipeName, runningHarness.ProbedPipeName);
        AssertLogMessage(runningLog, "engine: pipe (probe succeeded)");

        var startHarness = new Harness
        {
            State = EngineServiceState.Stopped,
            Compatible = true,
            StartResult = true,
        };
        var (started, startedLog) = ResolveAndCapture([], startHarness);
        Assert.Same(startHarness.PipeClient, started);
        Assert.Equal(0, startHarness.ProbeCalls);
        AssertLogMessage(
            startedLog,
            "engine: pipe (started marker-compatible on-demand service)");

        var startFailedHarness = new Harness
        {
            State = EngineServiceState.Stopped,
            Compatible = true,
        };
        var (startFailed, startFailedLog) = ResolveAndCapture([], startFailedHarness);
        Assert.IsType<UnavailableEngineClient>(startFailed);
        AssertLogMessage(
            startFailedLog,
            "engine: compatible on-demand service failed to start — setup required");

        var rejectedHarness = new Harness
        {
            State = EngineServiceState.Running,
            ProbeResult = false,
        };
        var (rejected, rejectedLog) = ResolveAndCapture([], rejectedHarness);
        Assert.IsType<UnavailableEngineClient>(rejected);
        AssertLogMessage(
            rejectedLog,
            "engine: service running but unreachable (token rejected) — unavailable");

        var incompatibleHarness = new Harness
        {
            State = EngineServiceState.Stopped,
            Compatible = false,
        };
        var (incompatible, incompatibleLog) = ResolveAndCapture([], incompatibleHarness);
        Assert.IsType<UnavailableEngineClient>(incompatible);
        AssertLogMessage(
            incompatibleLog,
            "engine: installed service protocol is incompatible — setup required");

        var absentHarness = new Harness();
        var (absent, absentLog) = ResolveAndCapture([], absentHarness);
        Assert.IsType<UnavailableEngineClient>(absent);
        Assert.Contains("elevated=false", absentLog, StringComparison.Ordinal);
        AssertLogMessage(
            absentLog,
            "engine: unavailable (no service registered) — setup required");
    }

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

        Assert.Contains(
            "unsupported --engine mode 'typo' (expected auto, pipe, or inproc)",
            error.Message,
            StringComparison.Ordinal);
    }

    [Fact]
    public void Resolve_rejects_duplicate_or_conflicting_engine_overrides()
    {
        var duplicate = Assert.Throws<ArgumentException>(
            () => EngineClientFactory.Resolve(["--engine=auto", "--ENGINE=pipe"]));
        Assert.Contains("specify --engine at most once", duplicate.Message, StringComparison.Ordinal);

        var conflict = Assert.Throws<ArgumentException>(
            () => EngineClientFactory.Resolve(["--fake-engine", "--engine=auto"]));
        Assert.Contains(
            "--fake-engine and --engine are mutually exclusive",
            conflict.Message,
            StringComparison.Ordinal);
    }

    [Fact]
    public void Resolve_injected_core_rejects_null_inputs()
    {
        var harness = new Harness();

        var args = Assert.Throws<ArgumentNullException>(
            () => EngineClientFactory.Resolve(null!, harness.Hooks));
        Assert.Equal("args", args.ParamName);

        var hooks = Assert.Throws<ArgumentNullException>(
            () => EngineClientFactory.Resolve([], null!));
        Assert.Equal("hooks", hooks.ParamName);

        var resolve = Assert.Throws<ArgumentNullException>(() =>
        {
            _ = EngineClientFactory.ResolveAsync((Func<IEngineClient>)null!);
        });
        Assert.Equal("resolve", resolve.ParamName);
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
