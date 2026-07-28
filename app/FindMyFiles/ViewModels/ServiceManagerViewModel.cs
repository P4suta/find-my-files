using System.Diagnostics.CodeAnalysis;
using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// The service-manager dialog's state: the read-only SCM state line plus the
/// lifecycle actions the gear menu's "Manage service…" exposes. Start/stop/
/// restart use the service object's narrow unelevated grants; only register and
/// uninstall use one-action UAC. UI thread only — blocking work runs on the
/// thread pool. The state flags (Is*/Can*) drive which controls are shown.
/// </summary>
internal sealed partial class ServiceManagerViewModel : ObservableObject
{
    /// <summary>fmf-service.exe (bundle or dev tree), resolved once. Needed only
    /// for elevated setup/removal; ordinary lifecycle control calls SCM directly.</summary>
    private readonly string? _exe;

    /// <summary>The wait-for-pipe-then-relaunch step after a successful elevated
    /// register/start, injected so the post-register flow is testable without a
    /// real service or exiting the process. Defaults to
    /// <see cref="ServiceProvisioner.Real"/>.</summary>
    private readonly ServiceProvisioner _provisioner;
    private readonly Action _restartApp;
    private readonly Action _exitApp;
    private readonly Func<EngineServiceState> _queryState;

    /// <summary>The read-only SCM state line (not installed / stopped /
    /// running (PID …) / unavailable / tool not found). Recomputed by
    /// <see cref="Refresh"/>.</summary>
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

    /// <summary>A service action is in flight — greys the action row
    /// (<see cref="NotBusy"/>) so operations can't overlap.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(NotBusy))]
    public partial bool Busy { get; set; }

    /// <summary>True while the explicit destructive confirmation for
    /// <c>uninstall --purge-data</c> is visible.</summary>
    [ObservableProperty]
    public partial bool PurgeConfirmationVisible { get; set; }

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

    /// <summary>SCM state could not be read. Only repair/re-registration is
    /// offered; lifecycle and destructive actions fail closed.</summary>
    [ObservableProperty]
    public partial bool IsUnknown { get; set; }

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

    /// <summary>Full cleanup applies whenever the bundled service tool exists and
    /// SCM reports a known state. Unlike service-only uninstall, it deliberately
    /// remains available after the service is gone so a failed per-user data
    /// deletion can be retried in-app.</summary>
    [ObservableProperty]
    public partial bool CanPurgeData { get; set; }

    /// <summary>The service tool is available (gates the register group). The
    /// accent "register and start" vs plain "re-register" split is by Is(Not)Installed.</summary>
    [ObservableProperty]
    public partial bool CanRegister { get; set; }

    /// <summary>Re-registration applies to a known installation or an unreadable
    /// state, where the elevated idempotent ritual is the recovery path.</summary>
    [ObservableProperty]
    public partial bool CanReregister { get; set; }

    /// <summary>Buttons stay enabled only while idle.</summary>
    public bool NotBusy => !Busy;

    /// <summary>Whether the result InfoBar has anything to show
    /// (<see cref="ResultText"/> non-empty).</summary>
    public bool HasResult => !string.IsNullOrEmpty(ResultText);

    /// <summary>True once the complete purge succeeded and the app-exit callback
    /// was requested. The dialog close path uses this to suppress its normal
    /// post-uninstall soft restart.</summary>
    public bool FullUninstallCompleted { get; private set; }

    /// <summary>Locates <c>fmf-service.exe</c> once (bundle or dev tree); the
    /// dialog should call <see cref="Refresh"/> on open to fill the state line.</summary>
    /// <param name="provisioner">The post-register wait+relaunch steps; defaults to
    /// <see cref="ServiceProvisioner.Real"/> (tests inject a fake).</param>
    /// <param name="restartApp">Soft-restart callback after transport changes;
    /// defaults to <see cref="App.SoftRestart"/>.</param>
    /// <param name="exitApp">Clean application exit after a successful full
    /// uninstall; defaults to <see cref="App.ExitApplication"/>.</param>
    /// <param name="queryState">Read-only SCM state seam for deterministic tests.</param>
    /// <param name="locateServiceExe">Service-tool lookup seam for deterministic tests.</param>
    public ServiceManagerViewModel(
        ServiceProvisioner? provisioner = null,
        Action? restartApp = null,
        Action? exitApp = null,
        Func<EngineServiceState>? queryState = null,
        Func<string?>? locateServiceExe = null)
    {
        _exe = (locateServiceExe
            ?? (() => ServiceSetup.LocateServiceExe(AppContext.BaseDirectory)))();
        _provisioner = provisioner ?? ServiceProvisioner.Real;
        _restartApp = restartApp ?? App.SoftRestart;
        _exitApp = exitApp ?? App.ExitApplication;
        _queryState = queryState ?? ServiceSetup.QueryState;
    }

    /// <summary>Re-read the SCM state and recompute which actions apply. Cheap
    /// read-only P/Invoke (no elevation) — safe on the UI thread.</summary>
    public void Refresh()
    {
        ApplyState(_queryState());
    }

    /// <summary>Apply one conservative SCM state to the visible/action state
    /// machine. Split from <see cref="Refresh"/> so fail-closed routing is
    /// deterministic without a real SCM.</summary>
    /// <param name="state">Resolved SCM state.</param>
    internal void ApplyState(EngineServiceState state)
    {
        IsRunning = state == EngineServiceState.Running;
        IsStopped = state == EngineServiceState.Stopped;
        IsNotInstalled = state == EngineServiceState.NotInstalled;
        IsUnknown = state == EngineServiceState.Unknown;
        IsInstalled = IsRunning || IsStopped;
        StateText = state switch
        {
            EngineServiceState.NotInstalled => Loc.Get("Svc_StateUnregistered"),
            EngineServiceState.Stopped => Loc.Get("Svc_StateStopped"),
            EngineServiceState.Running => FormatRunning(),
            _ => Loc.Get("Svc_StateUnavailable"),
        };
        var hasServiceTool = _exe is not null;
        CanRegister = hasServiceTool;
        CanReregister = hasServiceTool && (IsInstalled || IsUnknown);
        CanStart = state == EngineServiceState.Stopped;
        CanStop = state == EngineServiceState.Running;
        CanRestart = state == EngineServiceState.Running;
        CanUninstall = hasServiceTool && IsInstalled;
        CanPurgeData = hasServiceTool && (IsInstalled || IsNotInstalled);
    }

    private static string FormatRunning()
    {
        var pid = ServiceSetup.QueryServiceProcessId();
        return pid != 0 ? Loc.Get("Svc_StateRunningPid", pid) : Loc.Get("Svc_StateRunning");
    }

    /// <summary>Start the stopped service without UAC and verify this build's pipe.</summary>
    /// <returns>A task that completes after control and compatibility verification.</returns>
    public Task StartAsync() => RunControlAsync(
        "start", ServiceSetup.TryStartUnelevated, Loc.Get("Svc_Started"), verifyPipe: true);

    /// <summary>Stop the running service without UAC.</summary>
    /// <returns>A task that completes when the service has stopped.</returns>
    public Task StopAsync() => RunControlAsync(
        "stop", ServiceSetup.TryStopUnelevated, Loc.Get("Svc_Stopped"), verifyPipe: false);

    /// <summary>Restart the service without UAC and verify this build's pipe.</summary>
    /// <returns>A task that completes after restart and compatibility verification.</returns>
    public Task RestartAsync() => RunControlAsync(
        "restart", ServiceSetup.TryRestartUnelevated, Loc.Get("Svc_Restarted"), verifyPipe: true);

    /// <summary>install (idempotent) + restart in one elevated step (the
    /// fmf-service `setup` verb). The daily user's SID is forwarded so OTS
    /// elevation — a *different* admin account at the UAC prompt — does not
    /// lock this user out of the pipe (docs/SECURITY.md threat 1). The app is
    /// unelevated here, so CurrentUserSid is exactly that daily user.</summary>
    /// <returns>A task that completes when the elevated <c>setup</c> verb finishes.</returns>
    public Task RegisterAsync()
    {
        if (!ServiceSetup.TryCreateSetupArguments(out var args))
        {
            FileLog.Warn(
                "service-ui",
                "current user SID unavailable or invalid — refusing owner-less elevated setup");
            ResultSeverity = NotifySeverity.Error;
            ResultText = Loc.Get("Svc_IdentityUnavailable");
            return Task.CompletedTask;
        }

        return RunElevatedAsync(args, Loc.Get("Svc_Registered"));
    }

    /// <summary>Reveal the in-dialog confirmation for the irreversible
    /// machine-wide data purge. This remains valid when the service is already
    /// absent so a previous per-user deletion failure can be retried. A nested
    /// ContentDialog is not legal in WinUI, so the service manager owns this
    /// explicit confirmation surface.</summary>
    public void RequestPurgeConfirmation()
    {
        if (CanPurgeData && !Busy)
        {
            PurgeConfirmationVisible = true;
        }
    }

    /// <summary>Dismiss the purge confirmation without changing service state.</summary>
    public void CancelPurgeConfirmation() => PurgeConfirmationVisible = false;

    /// <summary>Uninstall the service through <see cref="ServiceProvisioner"/>.
    /// Purge mode maps exactly to <c>fmf-service uninstall --purge-data</c> and
    /// removes both machine-wide engine data and per-user UI data. Service-only
    /// removal soft-restarts the page graph in-process (ADR-0036); a successful
    /// full purge exits so the just-deleted user-data tree is not recreated.</summary>
    /// <param name="purgeData">Whether to remove machine-wide engine data too.</param>
    /// <returns>A task that completes after the elevated action and soft restart.</returns>
    public async Task UninstallAsync(bool purgeData)
    {
        var actionAllowed = purgeData ? CanPurgeData : CanUninstall;
        if (_exe is null || Busy || !actionAllowed)
        {
            return;
        }

        PurgeConfirmationVisible = false;
        Busy = true;
        ResultText = string.Empty;
        NeedsAppRestart = false;
        var exitAfterPurge = false;
        try
        {
            var result = await _provisioner.UninstallAsync(purgeData);
            if (result.Service.Outcome == ServiceActionOutcome.Ok
                && purgeData
                && !result.UserDataPurged)
            {
                ResultSeverity = NotifySeverity.Error;
                ResultText = Loc.Get("Svc_UserDataPurgeFailed");
            }
            else
            {
                (ResultSeverity, ResultText) = result.Service.Outcome switch
                {
                    ServiceActionOutcome.Ok => (
                        NotifySeverity.Info,
                        Loc.Get(purgeData ? "Svc_UninstalledWithData" : "Svc_Uninstalled")),
                    ServiceActionOutcome.Cancelled => (
                        NotifySeverity.Warning,
                        Loc.Get("Svc_Cancelled")),
                    _ => (
                        NotifySeverity.Error,
                        Loc.Get("Svc_Failed", result.Service.ExitCode)),
                };
            }

            if (result.Service.Outcome == ServiceActionOutcome.Ok)
            {
                if (purgeData && result.UserDataPurged)
                {
                    exitAfterPurge = true;
                }
                else if (!purgeData)
                {
                    _restartApp();
                }
            }
        }
        finally
        {
            Busy = false;
            if (!exitAfterPurge)
            {
                Refresh();
            }
        }

        if (exitAfterPurge)
        {
            FullUninstallCompleted = true;
            _exitApp();
        }
    }

    /// <summary>In-process soft restart so the rebuilt page connects to the
    /// now-running service over the pipe (ADR-0036).</summary>
    [SuppressMessage("Performance", "CA1822:Mark members as static", Justification = "x:Bind event/command target must remain an instance method")]
    public void RestartApp() => _restartApp();

    private async Task RunControlAsync(
        string verb,
        Func<bool> control,
        string okText,
        bool verifyPipe)
    {
        if (Busy)
        {
            return;
        }

        Busy = true;
        ResultText = string.Empty;
        NeedsAppRestart = false;
        try
        {
            var ok = await Task.Run(control);
            if (ok && verifyPipe)
            {
                ok = await Task.Run(
                    () => ServiceSetup.WaitForCompatibleStartedService(
                        PipeProtocol.DefaultPipeName));
                if (!ok)
                {
                    await Task.Run(ServiceSetup.TryStopUnelevated);
                    ResultSeverity = NotifySeverity.Warning;
                    ResultText = Loc.Get("Svc_Incompatible");
                }
            }

            if (ok)
            {
                ResultSeverity = NotifySeverity.Info;
                ResultText = okText;
            }
            else if (string.IsNullOrEmpty(ResultText))
            {
                ResultSeverity = NotifySeverity.Error;
                ResultText = Loc.Get("Svc_ControlFailed");
            }

            FileLog.Event(
                "service-ui",
                "unelevated service action completed",
                ("verb", verb),
                ("ok", ok));
        }
        finally
        {
            Busy = false;
            Refresh();
        }
    }

    private async Task RunElevatedAsync(string args, string okText)
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
            // Resume on the dispatcher (no ConfigureAwait) because the continuation sets bound
            // ResultSeverity / ResultText / Busy and calls Refresh(). This method is
            // invoked from the dialog's UI-thread commands.
            var result = await Task.Run(() => ServiceSetup.RunElevated(_exe, args));
            var verb = args.Split(' ', 2)[0];
            (ResultSeverity, ResultText) = result.Outcome switch
            {
                ServiceActionOutcome.Ok => (NotifySeverity.Info, okText),
                ServiceActionOutcome.Cancelled => (NotifySeverity.Warning, Loc.Get("Svc_Cancelled")),
                _ => (NotifySeverity.Error, Loc.Get("Svc_Failed", result.ExitCode)),
            };
            FileLog.Event(
                "service-ui",
                "service action completed",
                ("outcome", (int)result.Outcome),
                ("exit", result.ExitCode));

            // A successful setup changes the service executable/configuration,
            // so every current transport (unavailable, in-proc, or an older
            // pipe) must be re-resolved onto the freshly started service.
            if (result.Outcome == ServiceActionOutcome.Ok
                && string.Equals(verb, "setup", StringComparison.Ordinal))
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
