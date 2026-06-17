using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>Outcome of the auto-mode engine decision (no explicit
/// <c>--engine</c> / settings) — which transport to construct.</summary>
internal enum EngineChoice
{
    /// <summary>The service pipe answered the probe.</summary>
    Pipe,

    /// <summary>No live service and the process is elevated — in-proc FFI.</summary>
    Ffi,

    /// <summary>Service is running but rejected our token (stale authorized-SID
    /// list) — degrade to the empty engine; the setup screen recovers.</summary>
    EmptyServiceUnreachable,

    /// <summary>No live service and not elevated — degrade to the empty engine
    /// (no auto-runas); the setup screen offers the one-click install.</summary>
    EmptyNotElevated,

    /// <summary>No live service, not elevated, but the user has configured scope
    /// roots (ADR-0024) — run the non-elevated folder-walk engine in-proc over
    /// those roots (the corporate-PC path where admin is forbidden).</summary>
    WalkInProc,
}

/// <summary>
/// Engine transport selection, in priority order: CLI flags (--fake-engine /
/// --engine=pipe|inproc / --pipe-name=…) > settings.json "engine" > auto.
/// Auto probes the service pipe for 250ms (through Hello) and falls back to
/// the in-proc FFI engine when no service answers.
/// </summary>
public static class EngineClientFactory
{
    private static readonly TimeSpan ProbeTimeout = TimeSpan.FromMilliseconds(250);

    /// <summary>Called once at startup; resolves and returns a single engine
    /// implementation by the priority above. When in-proc is unavailable (no
    /// service plus not elevated), degrades to the empty engine
    /// (<see cref="FakeEngineClient.CreateEmpty"/>) and steers the UI to the
    /// setup screen (no auto-runas).</summary>
    /// <param name="args">Process command-line args (reads `--fake-engine` /
    /// `--engine=` / `--pipe-name=`).</param>
    /// <returns>The single chosen <see cref="IEngineClient"/> implementation instance.</returns>
    public static IEngineClient Resolve(string[] args)
    {
        var dataRoot = AppPaths.IsPortable
            ? $"data root: portable ({AppPaths.PortableRoot})"
            : "data root: per-user profile (app folder not writable)";
        FileLog.Info("app", dataRoot);
        if (HasFlag(args, "--fake-engine"))
        {
            FileLog.Info("app", "engine: fake (--fake-engine)");
            return new FakeEngineClient();
        }

        var pipeName = OptionValue(args, "--pipe-name=") ?? PipeProtocol.DefaultPipeName;
        var settings = AppSettings.Load();
        var mode = OptionValue(args, "--engine=") ?? settings.Engine;
        if (string.Equals(mode, "pipe", StringComparison.OrdinalIgnoreCase))
        {
            FileLog.Info("app", $"engine: pipe ({pipeName})");
            return new PipeEngineClient(pipeName);
        }

        if (string.Equals(mode, "inproc", StringComparison.OrdinalIgnoreCase))
        {
            FileLog.Info("app", "engine: in-proc FFI (explicit)");
            return new FfiEngineClient();
        }

        if (!string.Equals(mode, "auto", StringComparison.OrdinalIgnoreCase))
        {
            FileLog.Warn(
                "app",
                $"unknown engine mode `{mode}` (allowed: pipe | inproc | auto) — using auto");
        }

        // auto (or unknown mode → auto): probe the service pipe, else fall back
        // by service state + elevation. The decision table is unit-tested via
        // DecideAuto without touching the SCM, the pipe, or the token.
        var choice = DecideAuto(
            () => PipeEngineClient.Probe(pipeName, ProbeTimeout),
            ServiceSetup.QueryState,
            ServiceSetup.IsProcessElevated,
            () => settings.ScopeRoots.Length > 0);
        if (choice == EngineChoice.Pipe)
        {
            FileLog.Info("app", $"engine: pipe ({pipeName}, probe succeeded)");
            return new PipeEngineClient(pipeName);
        }

        if (choice == EngineChoice.Ffi)
        {
            // Service absent or stopped → the writer lock is free for in-proc.
            FileLog.Info("app", "engine: in-proc FFI (no live service, process is elevated)");
            return new FfiEngineClient();
        }

        if (choice == EngineChoice.EmptyServiceUnreachable)
        {
            // Running, but our token isn't on its authorized-SID list (a stale
            // list baked in at startup, or a foreign installer SID); in-proc would
            // die FMF_E_LOCKED. The setup screen (MainViewModel.IsDisconnected)
            // owns the recovery (re-register), so no separate notification here.
            FileLog.Warn(
                "app", "engine: service running but unreachable (token rejected) — empty fallback");
            return FakeEngineClient.CreateEmpty();
        }

        if (choice == EngineChoice.WalkInProc)
        {
            // Not elevated, no service, but scope roots are configured (ADR-0024):
            // run the folder-walk engine in-proc over the user's index at
            // %LOCALAPPDATA% — the corporate-PC path where admin is forbidden.
            FileLog.Info(
                "app",
                $"engine: scope walk in-proc ({settings.ScopeRoots.Length} roots, {settings.ScopeExcludes.Length} excludes, not elevated)");
            return FfiEngineClient.CreateScope(settings.ScopeRoots, settings.ScopeExcludes);
        }

        // EmptyNotElevated: no live service and not elevated. In-proc would fail
        // at the MFT read; degrade to the empty engine (no auto-runas) so the
        // setup screen can offer the one-click install (which leads with admin).
        FileLog.Warn("app", "engine: empty fallback (no service answered, not elevated)");
        return FakeEngineClient.CreateEmpty();
    }

    /// <summary>The auto-mode decision: probe the pipe, else fall back by service
    /// state, elevation, and scope config. Pure over the four injected probes so
    /// the branch table is unit-testable. Short-circuits: a successful probe never
    /// consults the rest; a running service never consults elevation; an elevated
    /// process never consults scope config.</summary>
    /// <param name="probe">Pipe probe — did a Hello round-trip succeed?</param>
    /// <param name="serviceState">SCM state of the engine service.</param>
    /// <param name="elevated">Whether this process is elevated.</param>
    /// <param name="hasScopeConfig">Whether the user configured scope roots
    /// (ADR-0024) — consulted only when not elevated and no service.</param>
    /// <returns>The transport to construct.</returns>
    internal static EngineChoice DecideAuto(
        Func<bool> probe,
        Func<EngineServiceState> serviceState,
        Func<bool> elevated,
        Func<bool> hasScopeConfig)
    {
        if (probe())
        {
            return EngineChoice.Pipe;
        }

        if (serviceState() == EngineServiceState.Running)
        {
            return EngineChoice.EmptyServiceUnreachable;
        }

        if (elevated())
        {
            return EngineChoice.Ffi;
        }

        // Not elevated, no service: walk the user's chosen roots if any are
        // configured (ADR-0024), else send them to the setup screen (which
        // leads with the admin path). `hasScopeConfig` is consulted last.
        return hasScopeConfig() ? EngineChoice.WalkInProc : EngineChoice.EmptyNotElevated;
    }

    internal static bool HasFlag(string[] args, string flag) =>
        args.Any(a => a.Equals(flag, StringComparison.OrdinalIgnoreCase));

    internal static string? OptionValue(string[] args, string prefix) =>
        args.FirstOrDefault(a => a.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
            ?[prefix.Length..];
}
