using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>Outcome of the auto-mode engine decision (no explicit
/// <c>--engine</c> / settings) — which transport to construct.</summary>
internal enum EngineChoice
{
    /// <summary>The service pipe answered the probe.</summary>
    Pipe,

    /// <summary>The service is installed but stopped — start it unelevated and
    /// connect over the pipe (on-demand lifecycle, ADR-0027). Resolved inside
    /// <see cref="EngineClientFactory.Resolve"/>; never surfaced to the UI.</summary>
    StartThenPipe,

    /// <summary>No live service and the process is elevated — in-proc FFI.</summary>
    Ffi,

    /// <summary>Service is running but rejected our token (stale authorized-SID
    /// list) — expose the explicit unavailable state; setup owns recovery.</summary>
    UnavailableServiceRejected,

    /// <summary>The stopped service does not advertise this build's protocol —
    /// do not start it; the setup screen re-registers it.</summary>
    UnavailableServiceIncompatible,

    /// <summary>No live service and not elevated — expose the explicit unavailable
    /// state (no auto-runas); setup offers the one-click install.</summary>
    UnavailableNotElevated,
}

/// <summary>
/// Engine transport selection, in priority order: explicit production mode
/// (<c>--engine=pipe|inproc</c>) then auto.
/// Auto checks the SCM first: a definitively absent/stopped service never pays
/// a pipe timeout; a live or unreadable service gets one bounded Hello probe.
/// Deterministic fake/unavailable engines and custom pipe names exist only when the
/// app is compiled with <c>FMF_TEST_SEAMS</c>; stable artifacts contain no
/// parser or string surface for those test-only switches.
/// </summary>
internal static class EngineClientFactory
{
    private static readonly TimeSpan ProbeTimeout = TimeSpan.FromMilliseconds(250);

    /// <summary>Called once at startup; resolves and returns a single engine
    /// implementation by the priority above. When in-proc is unavailable (no
    /// service plus not elevated), returns an explicit
    /// <see cref="UnavailableEngineClient"/> and steers the UI to the
    /// setup screen (no auto-runas).</summary>
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
            FileLog.Info("app", "engine: in-proc FFI (explicit)");
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
        var elevated = ServiceSetup.IsProcessElevated;
#if FMF_TEST_SEAMS
        var choice = pipeOverride is not null
            ? DecideCustomPipe(probe, elevated)
            : DecideAuto(
                ServiceSetup.QueryState,
                probe,
                elevated,
                ServiceSetup.IsInstalledServiceCompatible);
#else
        var choice = DecideAuto(
            ServiceSetup.QueryState,
            probe,
            elevated,
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

        if (choice == EngineChoice.Ffi)
        {
            // Service absent or stopped → the writer lock is free for in-proc.
            FileLog.Info("app", "engine: in-proc FFI (no live service, process is elevated)");
            return new FfiEngineClient();
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

        // UnavailableNotElevated: no live service and not elevated. In-proc would
        // fail at the MFT read; setup offers the one-click install.
        FileLog.Warn("app", "engine: unavailable (no service answered, not elevated)");
        return new UnavailableEngineClient();
    }

    /// <summary>The default-service auto decision. Query SCM first so absent and
    /// stopped services avoid a doomed pipe timeout. A non-stopped or unreadable
    /// service probes the pipe and fails closed when it cannot be reached.</summary>
    /// <param name="serviceState">SCM state of the engine service.</param>
    /// <param name="probe">Pipe probe — did a Hello round-trip succeed?</param>
    /// <param name="elevated">Whether this process is elevated.</param>
    /// <param name="serviceCompatible">Whether a stopped installation carries
    /// this build's exact SCM protocol marker.</param>
    /// <returns>The transport to construct.</returns>
    internal static EngineChoice DecideAuto(
        Func<EngineServiceState> serviceState,
        Func<bool> probe,
        Func<bool> elevated,
        Func<bool> serviceCompatible)
    {
        return serviceState() switch
        {
            EngineServiceState.Running or EngineServiceState.Unknown =>
                probe() ? EngineChoice.Pipe : EngineChoice.UnavailableServiceRejected,
            EngineServiceState.Stopped when serviceCompatible() => EngineChoice.StartThenPipe,
            EngineServiceState.Stopped => EngineChoice.UnavailableServiceIncompatible,
            EngineServiceState.NotInstalled => WithoutService(elevated),
            _ => EngineChoice.UnavailableServiceRejected,
        };
    }

#if FMF_TEST_SEAMS
    /// <summary>Decision for an explicit custom pipe, which has no SCM state to
    /// consult. A failed bounded probe falls back to the ordinary no-service
    /// path.</summary>
    /// <param name="probe">Custom-pipe Hello probe.</param>
    /// <param name="elevated">Whether this process is elevated.</param>
    /// <returns>The transport to construct.</returns>
    internal static EngineChoice DecideCustomPipe(
        Func<bool> probe,
        Func<bool> elevated) =>
        probe() ? EngineChoice.Pipe : WithoutService(elevated);
#endif

    /// <summary>The transport when no service is available: in-proc FFI if
    /// elevated, otherwise the explicit unavailable state that leads to one-time
    /// service setup. Also used when an on-demand start cannot be performed.</summary>
    /// <param name="elevated">Whether this process is elevated.</param>
    /// <returns>The transport to construct when the service is absent/unstartable.</returns>
    internal static EngineChoice WithoutService(Func<bool> elevated) =>
        elevated() ? EngineChoice.Ffi : EngineChoice.UnavailableNotElevated;

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
