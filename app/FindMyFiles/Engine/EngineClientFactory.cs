using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>Outcome of the auto-mode engine decision (no explicit
/// <c>--engine</c> / settings) — which transport to construct.
/// <para>Deliberately has no in-proc member: auto mode never selects the in-proc
/// engine, whatever the process token looks like. The in-proc engine creates
/// <c>%ProgramData%\find-my-files</c> itself, without the hardened descriptor
/// that only service install applies (docs/SECURITY.md threat 7), so an elevated
/// launch without a service would silently publish the machine index — every file
/// name on the box — under <c>C:\ProgramData</c>'s inherited permissive ACL.
/// Only the explicit <c>--engine=inproc</c> developer path builds it.</para></summary>
internal enum EngineChoice
{
    /// <summary>The service pipe answered the probe.</summary>
    Pipe,

    /// <summary>The service is installed but stopped — start it unelevated and
    /// connect over the pipe (on-demand lifecycle, ADR-0027). Resolved inside
    /// <see cref="EngineClientFactory.Resolve"/>; never surfaced to the UI.</summary>
    StartThenPipe,

    /// <summary>Service is running but rejected our token (stale authorized-SID
    /// list) — expose the explicit unavailable state; setup owns recovery.</summary>
    UnavailableServiceRejected,

    /// <summary>The stopped service does not advertise this build's protocol —
    /// do not start it; the setup screen re-registers it.</summary>
    UnavailableServiceIncompatible,

    /// <summary>No service to talk to — expose the explicit unavailable state
    /// (no auto-runas, no in-proc fallback); the setup screen offers the one-click
    /// install, which is exactly what an elevated first launch needs (ADR-0027:
    /// elevate once to install, then run unelevated).</summary>
    UnavailableNoService,
}

/// <summary>
/// Engine transport selection, in priority order: explicit production mode
/// (<c>--engine=pipe|inproc</c>) then auto.
/// Auto checks the SCM first: a definitively absent/stopped service never pays
/// a pipe timeout; a live or unreadable service gets one bounded Hello probe.
/// Auto only ever resolves to the pipe or to the explicit unavailable state —
/// the in-proc engine is reachable exclusively through <c>--engine=inproc</c>
/// (see <see cref="EngineChoice"/>).
/// Deterministic fake/unavailable engines and custom pipe names exist only when the
/// app is compiled with <c>FMF_TEST_SEAMS</c>; stable artifacts contain no
/// parser or string surface for those test-only switches.
/// </summary>
internal static class EngineClientFactory
{
    private static readonly TimeSpan ProbeTimeout = TimeSpan.FromMilliseconds(250);

    /// <summary>Called once at startup; resolves and returns a single engine
    /// implementation by the priority above. When no service can serve us,
    /// returns an explicit <see cref="UnavailableEngineClient"/> and steers the
    /// UI to the setup screen (no auto-runas, no in-proc fallback).</summary>
    /// <param name="args">Process command-line args (production reads only
    /// <c>--engine=pipe|inproc</c>).</param>
    /// <returns>The single chosen <see cref="IEngineClient"/> implementation instance.</returns>
    public static IEngineClient Resolve(string[] args)
    {
        FileLog.Event(
            "app",
            "data root selected",
            ("test_override", AppPaths.IsTestOverride));
        var modeOverrides = args
            .Where(a => a.StartsWith("--engine=", StringComparison.OrdinalIgnoreCase))
            .ToArray();
        if (modeOverrides.Length > 1)
        {
            throw new ArgumentException("specify --engine at most once", nameof(args));
        }

        var mode = modeOverrides.Length == 0
            ? "auto"
            : modeOverrides[0]["--engine=".Length..];
#if FMF_TEST_SEAMS
        if (HasFlag(args, "--fake-engine"))
        {
            if (modeOverrides.Length != 0)
            {
                throw new ArgumentException(
                    "--fake-engine and --engine are mutually exclusive",
                    nameof(args));
            }

            FileLog.Info("app", "engine: fake (--fake-engine)");
            return new FakeEngineClient();
        }

        if (string.Equals(mode, "unavailable", StringComparison.OrdinalIgnoreCase))
        {
            // Test seam (mirrors the always-available `--fake-engine`): force the
            // unavailable client so UI automation can drive the real disconnected setup
            // screen — `--fake-engine` returns the data-bearing fake, which never
            // enters the setup state (MainViewModel.IsDisconnected).
            FileLog.Info("app", "engine: unavailable (--engine=unavailable test seam)");
            return new UnavailableEngineClient();
        }

        var pipeOverride = OptionValue(args, "--pipe-name=");
        var pipeName = pipeOverride ?? PipeProtocol.DefaultPipeName;
#else
        const string pipeName = PipeProtocol.DefaultPipeName;
#endif
        if (string.Equals(mode, "pipe", StringComparison.OrdinalIgnoreCase))
        {
            FileLog.Info("app", "engine: pipe (explicit)");
            return new PipeEngineClient(pipeName);
        }

        if (string.Equals(mode, "inproc", StringComparison.OrdinalIgnoreCase))
        {
            WarnExplicitInProcIsUnhardened();
            return new FfiEngineClient();
        }

        if (!string.Equals(mode, "auto", StringComparison.OrdinalIgnoreCase))
        {
            throw new ArgumentException(
                $"unsupported --engine mode '{mode}' (expected auto, pipe, or inproc)",
                nameof(args));
        }

        // An explicit custom pipe is not represented by the fixed SCM service,
        // so probe it directly. The default path queries SCM first and only pays
        // the 250ms Hello budget when a service may actually be alive.
        var probe = () => PipeEngineClient.Probe(pipeName, ProbeTimeout);
#if FMF_TEST_SEAMS
        var choice = pipeOverride is not null
            ? DecideCustomPipe(probe)
            : DecideAuto(
                ServiceSetup.QueryState,
                probe,
                ServiceSetup.IsInstalledServiceCompatible);
#else
        var choice = DecideAuto(
            ServiceSetup.QueryState,
            probe,
            ServiceSetup.IsInstalledServiceCompatible);
#endif

        if (choice == EngineChoice.StartThenPipe)
        {
            // Installed but stopped: start it unelevated (the install granted this
            // user SERVICE_START — ADR-0027), then connect over the pipe as it
            // comes up (PipeEngineClient's supervisor retries until it answers).
            // If the start can't be done — e.g. an older install without the
            // granted right — fall back as if no service is present; the setup
            // screen's re-register then migrates it.
            if (ServiceSetup.TryStartUnelevated())
            {
                FileLog.Info("app", "engine: pipe (started marker-compatible on-demand service)");
                return new PipeEngineClient(pipeName);
            }

            FileLog.Warn(
                "app",
                "engine: compatible on-demand service failed to start — setup required");
            return new UnavailableEngineClient();
        }

        if (choice == EngineChoice.Pipe)
        {
            FileLog.Info("app", "engine: pipe (probe succeeded)");
            return new PipeEngineClient(pipeName);
        }

        if (choice == EngineChoice.UnavailableServiceRejected)
        {
            // Running, but our token isn't on its authorized-SID list (a stale
            // list baked in at startup, or a foreign installer SID); in-proc would
            // die FMF_E_LOCKED. The setup screen (MainViewModel.IsDisconnected)
            // owns the recovery (re-register), so no separate notification here.
            FileLog.Warn(
                "app", "engine: service running but unreachable (token rejected) — unavailable");
            return new UnavailableEngineClient();
        }

        if (choice == EngineChoice.UnavailableServiceIncompatible)
        {
            FileLog.Warn(
                "app",
                "engine: installed service protocol is incompatible — setup required");
            return new UnavailableEngineClient();
        }

        // UnavailableNoService: nothing is registered to talk to. Elevation is
        // deliberately not a fallback — an elevated launch lands on the same setup
        // screen, whose one-click install is the *supported* use of that token
        // (ADR-0027). `elevated` below is a support-diagnostic field only; it is
        // not an input to any decision above.
        FileLog.WarnEvent(
            "app",
            "engine: unavailable (no service registered) — setup required",
            ex: null,
            ("elevated", ServiceSetup.IsProcessElevated()));
        return new UnavailableEngineClient();
    }

    /// <summary>The explicit <c>--engine=inproc</c> developer path bypasses the
    /// service, so nothing has applied the hardened descriptor that service
    /// install puts on <c>%ProgramData%\find-my-files</c>: when this process is
    /// the first to create the tree, it inherits <c>C:\ProgramData</c>'s
    /// permissive ACL and the machine index — every file name on the box
    /// (docs/SECURITY.md threat 7) — becomes readable, and its directory
    /// pre-creatable, by every local standard user. Accepted for a path a
    /// developer opts into by hand, but never silently: say so on the way in,
    /// and say whether an install has ever hardened the root.</summary>
    private static void WarnExplicitInProcIsUnhardened()
    {
        const string message =
            "engine: in-proc FFI (explicit --engine=inproc) — the machine index is "
            + "outside the service-hardened data root";
        FileLog.WarnEvent(
            "app",
            message,
            ex: null,
            ("service_installed", ServiceSetup.QueryState() != EngineServiceState.NotInstalled));
    }

    /// <summary>Resolves the engine on a worker so synchronous SCM and pipe
    /// probes can never delay first-window activation or block the UI STA.</summary>
    /// <param name="args">Process engine-selection arguments.</param>
    /// <param name="ct">Cancels work that has not started; callers discard and
    /// dispose a result completed after cancellation.</param>
    /// <returns>The asynchronously resolved engine session.</returns>
    internal static Task<IEngineClient> ResolveAsync(
        string[] args,
        CancellationToken ct = default) =>
        ResolveAsync(() => Resolve(args), ct);

    /// <summary>Injected resolver seam for proving resolution never runs on the
    /// calling/UI thread.</summary>
    /// <param name="resolve">Synchronous engine resolution.</param>
    /// <param name="ct">Cancels work that has not started.</param>
    /// <returns>The asynchronously resolved engine session.</returns>
    internal static Task<IEngineClient> ResolveAsync(
        Func<IEngineClient> resolve,
        CancellationToken ct = default)
    {
        ArgumentNullException.ThrowIfNull(resolve);
        return Task.Run(resolve, ct);
    }

    /// <summary>The default-service auto decision. Query SCM first so absent and
    /// stopped services avoid a doomed pipe timeout. A non-stopped or unreadable
    /// service probes the pipe and fails closed when it cannot be reached.
    /// <para>Elevation is deliberately not a parameter: no combination of SCM
    /// state and token can make auto mode build the in-proc engine over an
    /// unhardened <c>%ProgramData%</c> tree (see <see cref="EngineChoice"/>).</para></summary>
    /// <param name="serviceState">SCM state of the engine service.</param>
    /// <param name="probe">Pipe probe — did a Hello round-trip succeed?</param>
    /// <param name="serviceCompatible">Whether a stopped installation carries
    /// this build's exact SCM protocol marker.</param>
    /// <returns>The transport to construct.</returns>
    internal static EngineChoice DecideAuto(
        Func<EngineServiceState> serviceState,
        Func<bool> probe,
        Func<bool> serviceCompatible)
    {
        return serviceState() switch
        {
            EngineServiceState.Running or EngineServiceState.Unknown =>
                probe() ? EngineChoice.Pipe : EngineChoice.UnavailableServiceRejected,
            EngineServiceState.Stopped when serviceCompatible() => EngineChoice.StartThenPipe,
            EngineServiceState.Stopped => EngineChoice.UnavailableServiceIncompatible,
            EngineServiceState.NotInstalled => EngineChoice.UnavailableNoService,
            _ => EngineChoice.UnavailableServiceRejected,
        };
    }

#if FMF_TEST_SEAMS
    /// <summary>Decision for an explicit custom pipe, which has no SCM state to
    /// consult. A failed bounded probe falls back to the ordinary no-service
    /// path.</summary>
    /// <param name="probe">Custom-pipe Hello probe.</param>
    /// <returns>The transport to construct.</returns>
    internal static EngineChoice DecideCustomPipe(Func<bool> probe) =>
        probe() ? EngineChoice.Pipe : EngineChoice.UnavailableNoService;
#endif

#if FMF_TEST_SEAMS
    internal static bool HasFlag(string[] args, string flag) =>
        args.Any(a => a.Equals(flag, StringComparison.OrdinalIgnoreCase));
#endif

    internal static string? OptionValue(string[] args, string prefix) =>
        args.FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            ?[prefix.Length..];

    /// <summary>Replace any existing engine override with one explicit mode.
    /// Used by in-process soft restart after service setup/stop.</summary>
    /// <param name="args">Original process arguments.</param>
    /// <param name="mode">Engine mode to append.</param>
    /// <returns>A copied argument array with exactly one engine override.</returns>
    internal static string[] WithEngineMode(string[] args, string mode) =>
    [
        .. args.Where(a => !a.StartsWith("--engine=", StringComparison.OrdinalIgnoreCase)),
        $"--engine={mode}",
    ];
}
