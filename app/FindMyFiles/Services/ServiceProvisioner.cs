namespace FindMyFiles.Services;

/// <summary>
/// The shared "make file search usable" steps, behind both the setup screen's
/// one-click button and the management dialog's register action: register the
/// fmf-engine service elevated, then re-resolve the engine in-process forcing the
/// pipe transport so the unavailable page comes back as a retrying pipe client.
/// <para>The engine transport is chosen once when the page is built, so an
/// in-process soft restart (rebuild the page, ADR-0036) is how a freshly
/// registered service takes effect. It forces <c>--engine=pipe</c> (not a re-run
/// of <c>auto</c>) precisely because the service it just started is still warming
/// up — auto's single short probe could miss it and report the engine unavailable,
/// leaving the user stuck on the setup screen. The pipe client's supervisor
/// instead waits out the warm-up with 250ms→5s backoff and the UI flips
/// Setup→Ready the moment it connects.</para>
/// <para>Instance-based so the elevated setup and the soft restart are injectable
/// boundaries (ADR-0022) — production code uses <see cref="Real"/>, which wires
/// the real statics; tests drive fakes.</para>
/// </summary>
internal sealed class ServiceProvisioner
{
    private static ServiceProvisionerHooks _hooks = ServiceProvisionerHooks.Production;
    private readonly Func<Task<ServiceActionOutcome>> _register;
    private readonly Action _relaunch;
    private readonly Func<bool, Task<ServiceActionResult>> _uninstall;
    private readonly Func<bool> _purgeUserData;

    /// <summary>Builds a provisioner over its lifecycle boundaries. Internal so only
    /// production (<see cref="Real"/>) and tests construct it; callers receive
    /// one by injection, defaulting to <see cref="Real"/>.</summary>
    /// <param name="register">The elevated install+start step (the fmf-service
    /// `setup` verb), returning its outcome.</param>
    /// <param name="relaunch">In-process soft restart forcing the pipe transport,
    /// so the rebuilt page binds the retrying pipe client.</param>
    /// <param name="uninstall">The elevated service removal step. The boolean
    /// selects the service's <c>--purge-data</c> mode.</param>
    /// <param name="purgeUserData">Deletes the UI-owned per-user state after a
    /// successful machine-wide purge.</param>
    internal ServiceProvisioner(
        Func<Task<ServiceActionOutcome>> register,
        Action relaunch,
        Func<bool, Task<ServiceActionResult>>? uninstall = null,
        Func<bool>? purgeUserData = null)
    {
        _register = register;
        _relaunch = relaunch;
        _uninstall = uninstall ?? UninstallElevatedAsync;
        _purgeUserData = purgeUserData ?? AppDataPurger.TryPurge;
    }

    /// <summary>The production provisioner: the real elevated setup and the
    /// in-process soft restart forcing the pipe transport
    /// (<see cref="App.SoftRestartIntoPipe"/>). Callers default to this.</summary>
    public static ServiceProvisioner Real { get; } = new(
        RegisterElevatedAsync,
        App.SoftRestartIntoPipe);

    /// <summary>Replace all production provisioning boundaries as one atomic
    /// test seam. The returned disposable restores the previous set.</summary>
    /// <param name="hooks">Complete deterministic boundary implementation.</param>
    /// <returns>A scope that restores the previous hooks.</returns>
    internal static IDisposable UseHooksForTests(ServiceProvisionerHooks hooks)
    {
        ArgumentNullException.ThrowIfNull(hooks);
        var previous = Interlocked.Exchange(ref _hooks, hooks);
        return new ActionOnDispose(() => Interlocked.Exchange(ref _hooks, previous));
    }

    /// <summary>install (idempotent) + restart in one elevated step (the
    /// fmf-service `setup` verb), forwarding the daily user's SID so OTS
    /// elevation doesn't lock them out (docs/SECURITY.md threat 1). Blocking
    /// work runs off the UI thread.</summary>
    /// <returns>The outcome of the elevated setup step (success, declined, or failed).</returns>
    public Task<ServiceActionOutcome> RegisterAsync() => _register();

    /// <summary>Unregister the service through one elevated helper action.
    /// <paramref name="purgeData"/> maps exactly to
    /// <c>fmf-service uninstall --purge-data</c>. After that succeeds, the full
    /// purge also removes the UI-owned <c>%APPDATA%</c> directory.</summary>
    /// <param name="purgeData">Also remove the machine-wide index, service
    /// settings, stable service binary, and engine logs.</param>
    /// <returns>The service result plus whether the requested user-data purge completed.</returns>
    public async Task<ServiceUninstallResult> UninstallAsync(bool purgeData)
    {
        var service = await _uninstall(purgeData).ConfigureAwait(false);
        var userDataPurged = !purgeData;
        if (purgeData && service.Outcome == ServiceActionOutcome.Ok)
        {
            userDataPurged = _purgeUserData();
        }

        return new ServiceUninstallResult(service, userDataPurged);
    }

    /// <summary>Re-resolve the engine in-process forcing the pipe transport so the
    /// rebuilt page binds a retrying pipe client to the just-registered service.
    /// Call only after <see cref="RegisterAsync"/> reports success — it assumes a
    /// service is now installed and starting.</summary>
    public void RelaunchIntoPipe() => _relaunch();

    /// <summary>The real elevated setup behind <see cref="Real"/>: locate
    /// fmf-service.exe, forward the SID-validated owner flag, and run the
    /// elevated `setup` verb off the UI thread.</summary>
    /// <returns>The outcome of the elevated setup, or Failed when the exe is missing.</returns>
    private static async Task<ServiceActionOutcome> RegisterElevatedAsync()
    {
        var hooks = Volatile.Read(ref _hooks);
        var exe = hooks.LocateServiceExe(AppContext.BaseDirectory);
        if (exe is null)
        {
            FileLog.Warn("service-ui", "fmf-service.exe not found — cannot register");
            return ServiceActionOutcome.Failed;
        }

        var setup = hooks.CreateSetupArguments();
        if (!setup.Success)
        {
            FileLog.Warn(
                "service-ui",
                "current user SID unavailable or invalid — refusing owner-less elevated setup");
            return ServiceActionOutcome.IdentityUnavailable;
        }

        var result = await Task.Run(
            () => hooks.RunElevated(exe, setup.Arguments)).ConfigureAwait(false);
        FileLog.Event(
            "service-ui",
            "service action completed",
            ("outcome", (int)result.Outcome),
            ("exit", result.ExitCode));
        return result.Outcome;
    }

    /// <summary>The real elevated uninstall behind <see cref="Real"/>. The
    /// command line is closed over a boolean, so no user-controlled text reaches
    /// the elevated boundary.</summary>
    /// <param name="purgeData">Whether the machine-owned ProgramData is removed.</param>
    /// <returns>The classified elevated helper result.</returns>
    internal static async Task<ServiceActionResult> UninstallElevatedAsync(bool purgeData)
    {
        var hooks = Volatile.Read(ref _hooks);
        var exe = hooks.LocateServiceExe(AppContext.BaseDirectory);
        if (exe is null)
        {
            FileLog.Warn("service-ui", "fmf-service.exe not found — cannot uninstall");
            return new ServiceActionResult(ServiceActionOutcome.Failed, -1);
        }

        var args = purgeData ? "uninstall --purge-data" : "uninstall";
        var result = await Task.Run(() => hooks.RunElevated(exe, args)).ConfigureAwait(false);
        FileLog.Event(
            "service-ui",
            "service action completed",
            ("verb", "uninstall"),
            ("purge_data", purgeData),
            ("outcome", (int)result.Outcome),
            ("exit", result.ExitCode));
        return result;
    }

    private sealed class ActionOnDispose(Action action) : IDisposable
    {
        private Action? _action = action;

        public void Dispose() => Interlocked.Exchange(ref _action, null)?.Invoke();
    }
}

/// <summary>Outcome of the two-scope full-uninstall sequence.</summary>
/// <param name="Service">Elevated service/ProgramData removal result.</param>
/// <param name="UserDataPurged">True when no user-data purge was requested or
/// when the AppData tree was removed successfully.</param>
internal readonly record struct ServiceUninstallResult(
    ServiceActionResult Service,
    bool UserDataPurged);
