using System.Diagnostics.CodeAnalysis;
using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// The service-manager dialog's state: the read-only SCM state line plus the
/// per-action UAC mutations the gear menu's "Manage service…" exposes — the one
/// place the app manages the fmf-engine service. Each action shells one
/// elevated fmf-service verb (<see cref="ServiceSetup.RunElevated"/>); the app
/// itself stays asInvoker. UI thread only — the work itself hops to a thread
/// pool. The state flags (Is*/Can*) drive which controls the dialog shows.
/// </summary>
public sealed partial class ServiceManagerViewModel : ObservableObject
{
    /// <summary>fmf-service.exe (bundle or dev tree), resolved once. Null
    /// disables every action and the state line says why.</summary>
    private readonly string? _exe;

    /// <summary>The wait-for-pipe-then-relaunch step after a successful elevated
    /// register/start, injected so the post-register flow is testable without a
    /// real service or exiting the process. Defaults to
    /// <see cref="ServiceProvisioner.Real"/>.</summary>
    private readonly ServiceProvisioner _provisioner;

    /// <summary>The read-only SCM state line (not installed / stopped /
    /// running (PID …) / tool not found). Recomputed by <see cref="Refresh"/>.</summary>
    [ObservableProperty]
    public partial string StateText { get; set; } = Loc.Get("Svc_StateChecking");

    /// <summary>InfoBar text for the last action's outcome; empty means no
    /// result bar (<see cref="HasResult"/>). Severity is <see cref="ResultSeverity"/>.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasResult))]
    public partial string ResultText { get; set; } = string.Empty;

    /// <summary>Severity of the last action's result InfoBar.</summary>
    [ObservableProperty]
    public partial NotifySeverity ResultSeverity { get; set; } = NotifySeverity.Info;

    /// <summary>An elevated action is in flight — greys the action row
    /// (<see cref="NotBusy"/>) so two UAC verbs can't overlap.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(NotBusy))]
    public partial bool Busy { get; set; }

    /// <summary>Uninstall checkbox: also wipe <c>%ProgramData%\find-my-files</c>
    /// (index + service.json) on uninstall, vs leaving the data in place.</summary>
    [ObservableProperty]
    public partial bool PurgeData { get; set; }

    /// <summary>Fallback only: shown when the automatic post-register relaunch
    /// couldn't confirm the service came up — surfaces the manual
    /// "Restart app" button.</summary>
    [ObservableProperty]
    public partial bool NeedsAppRestart { get; set; }

    // ── State, for the header icon + section visibility (set in Refresh) ──

    /// <summary>Service installed and running — drives the header "running" icon.</summary>
    [ObservableProperty]
    public partial bool IsRunning { get; set; }

    /// <summary>Service installed but stopped.</summary>
    [ObservableProperty]
    public partial bool IsStopped { get; set; }

    /// <summary>Service not installed at all — shows the register prompt.</summary>
    [ObservableProperty]
    public partial bool IsNotInstalled { get; set; }

    /// <summary>Installed (Stopped or Running) — gates the lifecycle/uninstall groups.</summary>
    [ObservableProperty]
    public partial bool IsInstalled { get; set; }

    // ── Which lifecycle buttons apply (set in Refresh) ──

    /// <summary>Start applies — service is installed and Stopped.</summary>
    [ObservableProperty]
    public partial bool CanStart { get; set; }

    /// <summary>Stop applies — service is Running.</summary>
    [ObservableProperty]
    public partial bool CanStop { get; set; }

    /// <summary>Restart applies — service is Running.</summary>
    [ObservableProperty]
    public partial bool CanRestart { get; set; }

    /// <summary>Uninstall applies — service is installed (Stopped or Running).</summary>
    [ObservableProperty]
    public partial bool CanUninstall { get; set; }

    /// <summary>The service tool is available (gates the register group). The
    /// accent "register and start" vs plain "re-register" split is by Is(Not)Installed.</summary>
    [ObservableProperty]
    public partial bool CanRegister { get; set; }

    /// <summary>Buttons stay enabled only while idle — an in-flight UAC action
    /// greys the whole row (visibility is still driven by the Is*/Can* flags).</summary>
    public bool NotBusy => !Busy;

    /// <summary>Whether the result InfoBar has anything to show
    /// (<see cref="ResultText"/> non-empty).</summary>
    public bool HasResult => !string.IsNullOrEmpty(ResultText);

    /// <summary>Locates <c>fmf-service.exe</c> once (bundle or dev tree); the
    /// dialog should call <see cref="Refresh"/> on open to fill the state line.</summary>
    /// <param name="provisioner">The post-register wait+relaunch steps; defaults to
    /// <see cref="ServiceProvisioner.Real"/> (tests inject a fake).</param>
    public ServiceManagerViewModel(ServiceProvisioner? provisioner = null)
    {
        _exe = ServiceSetup.LocateServiceExe(AppContext.BaseDirectory);
        _provisioner = provisioner ?? ServiceProvisioner.Real;
    }

    /// <summary>Re-read the SCM state and recompute which actions apply. Cheap
    /// read-only P/Invoke (no elevation) — safe on the UI thread.</summary>
    public void Refresh()
    {
        if (_exe is null)
        {
            StateText = Loc.Get("Svc_ExeNotFound");
            IsRunning = IsStopped = IsNotInstalled = IsInstalled = false;
            CanStart = CanStop = CanRestart = CanUninstall = CanRegister = false;
            return;
        }

        var state = ServiceSetup.QueryState();
        IsRunning = state == EngineServiceState.Running;
        IsStopped = state == EngineServiceState.Stopped;
        IsNotInstalled = state == EngineServiceState.NotInstalled;
        IsInstalled = state != EngineServiceState.NotInstalled;
        StateText = state switch
        {
            EngineServiceState.NotInstalled => Loc.Get("Svc_StateUnregistered"),
            EngineServiceState.Stopped => Loc.Get("Svc_StateStopped"),
            _ => FormatRunning(),
        };
        CanRegister = true;
        CanStart = state == EngineServiceState.Stopped;
        CanStop = state == EngineServiceState.Running;
        CanRestart = state == EngineServiceState.Running;
        CanUninstall = state != EngineServiceState.NotInstalled;
    }

    private static string FormatRunning()
    {
        var pid = ServiceSetup.QueryServiceProcessId();
        return pid != 0 ? Loc.Get("Svc_StateRunningPid", pid) : Loc.Get("Svc_StateRunning");
    }

    /// <summary>Start the stopped service (one elevated <c>start</c> verb).</summary>
    /// <returns>A task that completes when the elevated <c>start</c> verb finishes.</returns>
    public Task StartAsync() => RunAsync("start", Loc.Get("Svc_Started"));

    /// <summary>Stop the running service (one elevated <c>stop</c> verb).</summary>
    /// <returns>A task that completes when the elevated <c>stop</c> verb finishes.</returns>
    public Task StopAsync() => RunAsync("stop", Loc.Get("Svc_Stopped"));

    /// <summary>Restart the running service (one elevated <c>restart</c> verb).</summary>
    /// <returns>A task that completes when the elevated <c>restart</c> verb finishes.</returns>
    public Task RestartAsync() => RunAsync("restart", Loc.Get("Svc_Restarted"));

    /// <summary>install (idempotent) + restart in one elevated step (the
    /// fmf-service `setup` verb). The daily user's SID is forwarded so OTS
    /// elevation — a *different* admin account at the UAC prompt — does not
    /// lock this user out of the pipe (docs/SECURITY.md threat 1). The app is
    /// unelevated here, so CurrentUserSid is exactly that daily user.</summary>
    /// <returns>A task that completes when the elevated <c>setup</c> verb finishes.</returns>
    public Task RegisterAsync()
    {
        var sid = ServiceSetup.CurrentUserSid();
        var args = ServiceSetup.IsValidSid(sid) ? $"setup --owner-sid={sid}" : "setup";
        return RunAsync(args, Loc.Get("Svc_Registered"));
    }

    /// <summary>Uninstall the service (one elevated <c>uninstall</c> verb),
    /// adding <c>--purge-data</c> when <see cref="PurgeData"/> is set.</summary>
    /// <returns>A task that completes when the elevated <c>uninstall</c> verb finishes.</returns>
    public Task UninstallAsync() =>
        RunAsync(PurgeData ? "uninstall --purge-data" : "uninstall", Loc.Get("Svc_Uninstalled"));

    /// <summary>Plain (unelevated) relaunch so the fresh instance connects to
    /// the now-running service over the pipe.</summary>
    [SuppressMessage("Performance", "CA1822:Mark members as static", Justification = "x:Bind event/command target must remain an instance method")]
    public void RestartApp() => ShellOps.Relaunch();

    private async Task RunAsync(string args, string okText)
    {
        if (_exe is null || Busy)
        {
            return;
        }

        Busy = true;
        ResultText = string.Empty;
        NeedsAppRestart = false;
        try
        {
            // RunElevated already runs off the UI thread (Task.Run); resume on the
            // dispatcher (no ConfigureAwait) because the continuation sets bound
            // ResultSeverity / ResultText / Busy and calls Refresh(). RunAsync is
            // invoked from the dialog's UI-thread commands.
            var result = await Task.Run(() => ServiceSetup.RunElevated(_exe, args));
            var verb = args.Split(' ', 2)[0];
            (ResultSeverity, ResultText) = result.Outcome switch
            {
                ServiceActionOutcome.Ok => (NotifySeverity.Info, okText),
                ServiceActionOutcome.Cancelled => (NotifySeverity.Warning, Loc.Get("Svc_Cancelled")),
                _ => (NotifySeverity.Error, Loc.Get("Svc_Failed", result.ExitCode, verb)),
            };
            FileLog.Info("service-ui", $"`{args}` → {result.Outcome} (exit {result.ExitCode})");

            // Register/start succeeds, but this instance is still on the empty fake
            // engine (the transport is chosen once, at startup). Relaunch forcing
            // the pipe transport so the fresh instance binds a retrying pipe client
            // and rides out the just-started service's warm-up — the user shouldn't
            // have to. On success this process exits inside RelaunchIntoPipe; only a
            // failed relaunch (ShellOps notifies) falls through, where the
            // pre-armed "Restart app" button is the manual escape hatch.
            if (result.Outcome == ServiceActionOutcome.Ok
                && verb is "setup" or "start"
                && App.EngineClient is FakeEngineClient { IsEmpty: true })
            {
                ResultSeverity = NotifySeverity.Warning;
                ResultText = Loc.Get("Svc_RegisteredNotConfirmed");
                NeedsAppRestart = true;
                _provisioner.RelaunchIntoPipe();
            }
        }
        finally
        {
            Busy = false;
            Refresh();
        }
    }
}
