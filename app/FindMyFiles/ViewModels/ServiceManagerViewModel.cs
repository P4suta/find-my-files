using CommunityToolkit.Mvvm.ComponentModel;
using FindMyFiles.Engine;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// The service-manager dialog's state: the read-only SCM state line plus the
/// per-action UAC mutations the gear menu's「サービスの管理…」exposes — the one
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

    [ObservableProperty]
    public partial string StateText { get; set; } = Loc.Get("Svc_StateChecking");

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasResult))]
    public partial string ResultText { get; set; } = string.Empty;

    /// <summary>Severity of the last action's result InfoBar.</summary>
    [ObservableProperty]
    public partial NotifySeverity ResultSeverity { get; set; } = NotifySeverity.Info;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(NotBusy))]
    public partial bool Busy { get; set; }

    [ObservableProperty]
    public partial bool PurgeData { get; set; }

    /// <summary>Fallback only: shown when the automatic post-register relaunch
    /// couldn't confirm the service came up — surfaces the manual
    /// 「アプリを再起動」button.</summary>
    [ObservableProperty]
    public partial bool NeedsAppRestart { get; set; }

    // ── State, for the header icon + section visibility (set in Refresh) ──
    [ObservableProperty]
    public partial bool IsRunning { get; set; }

    [ObservableProperty]
    public partial bool IsStopped { get; set; }

    [ObservableProperty]
    public partial bool IsNotInstalled { get; set; }

    /// <summary>Installed (Stopped or Running) — gates the 稼働/削除 groups.</summary>
    [ObservableProperty]
    public partial bool IsInstalled { get; set; }

    // ── Which lifecycle buttons apply (set in Refresh) ──
    [ObservableProperty]
    public partial bool CanStart { get; set; }

    [ObservableProperty]
    public partial bool CanStop { get; set; }

    [ObservableProperty]
    public partial bool CanRestart { get; set; }

    [ObservableProperty]
    public partial bool CanUninstall { get; set; }

    /// <summary>The service tool is available (gates the 登録 group). The
    /// accent「登録して開始」vs plain「登録し直す」split is by Is(Not)Installed.</summary>
    [ObservableProperty]
    public partial bool CanRegister { get; set; }

    /// <summary>Buttons stay enabled only while idle — an in-flight UAC action
    /// greys the whole row (visibility is still driven by the Is*/Can* flags).</summary>
    public bool NotBusy => !Busy;

    public bool HasResult => !string.IsNullOrEmpty(ResultText);

    public ServiceManagerViewModel()
    {
        _exe = ServiceSetup.LocateServiceExe(AppContext.BaseDirectory);
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

    public Task StartAsync() => RunAsync("start", Loc.Get("Svc_Started"));

    public Task StopAsync() => RunAsync("stop", Loc.Get("Svc_Stopped"));

    public Task RestartAsync() => RunAsync("restart", Loc.Get("Svc_Restarted"));

    /// <summary>install (idempotent) + restart in one elevated step (the
    /// fmf-service `setup` verb). The daily user's SID is forwarded so OTS
    /// elevation — a *different* admin account at the UAC prompt — does not
    /// lock this user out of the pipe (docs/SECURITY.md 脅威1). The app is
    /// unelevated here, so CurrentUserSid is exactly that daily user.</summary>
    public Task RegisterAsync()
    {
        var sid = ServiceSetup.CurrentUserSid();
        var args = ServiceSetup.IsValidSid(sid) ? $"setup --owner-sid={sid}" : "setup";
        return RunAsync(args, Loc.Get("Svc_Registered"));
    }

    public Task UninstallAsync() =>
        RunAsync(PurgeData ? "uninstall --purge-data" : "uninstall", Loc.Get("Svc_Uninstalled"));

    /// <summary>Plain (unelevated) relaunch so the fresh instance connects to
    /// the now-running service over the pipe.</summary>
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
            var result = await Task.Run(() => ServiceSetup.RunElevated(_exe, args));
            var verb = args.Split(' ', 2)[0];
            (ResultSeverity, ResultText) = result.Outcome switch
            {
                ServiceActionOutcome.Ok => (NotifySeverity.Info, okText),
                ServiceActionOutcome.Cancelled => (NotifySeverity.Warning, Loc.Get("Svc_Cancelled")),
                _ => (NotifySeverity.Error, Loc.Get("Svc_Failed", result.ExitCode, verb)),
            };
            FileLog.Info("service-ui", $"`{args}` → {result.Outcome} (exit {result.ExitCode})");
            // Register/start succeeds, but this instance is still on the empty
            // fake engine (the transport is chosen once, at startup). Wait for
            // the service's pipe to come up, then relaunch automatically so the
            // fresh instance connects — the user shouldn't have to.
            if (result.Outcome == ServiceActionOutcome.Ok
                && verb is "setup" or "start"
                && App.EngineClient is FakeEngineClient { IsEmpty: true })
            {
                ResultSeverity = NotifySeverity.Info;
                ResultText = Loc.Get("Setup_Connecting");
                if (!await ServiceProvisioner.WaitForServiceThenRelaunchAsync())
                {
                    ResultSeverity = NotifySeverity.Warning;
                    ResultText = Loc.Get("Svc_RegisteredNotConfirmed");
                    NeedsAppRestart = true;
                }
            }
        }
        finally
        {
            Busy = false;
            Refresh();
        }
    }

}
