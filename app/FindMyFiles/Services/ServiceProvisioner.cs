namespace FindMyFiles.Services;

/// <summary>
/// The shared "make file search usable" steps, behind both the setup screen's
/// one-click button and the management dialog's register action: register the
/// fmf-engine service elevated, then relaunch (unelevated) forcing the pipe
/// transport so this empty-fake instance comes back as a retrying pipe client.
/// <para>The engine transport is chosen once at startup, so a relaunch is how a
/// freshly registered service takes effect. The relaunch forces
/// <c>--engine=pipe</c> (not a re-run of <c>auto</c>) precisely because the
/// service it just started is still warming up — auto's single short probe could
/// miss it and fall back to the empty engine, leaving the user stuck on the setup
/// screen. The pipe client's supervisor instead waits out the warm-up with
/// 250ms→5s backoff and the UI flips Setup→Ready the moment it connects.</para>
/// <para>Instance-based so the elevated setup and the relaunch are injectable
/// boundaries (ADR-0022) — production code uses <see cref="Real"/>, which wires
/// the real statics; tests drive fakes.</para>
/// </summary>
public sealed class ServiceProvisioner
{
    private readonly Func<Task<ServiceActionOutcome>> _register;
    private readonly Action _relaunch;

    /// <summary>Builds a provisioner over its two boundaries. Internal so only
    /// production (<see cref="Real"/>) and tests construct it; callers receive
    /// one by injection, defaulting to <see cref="Real"/>.</summary>
    /// <param name="register">The elevated install+start step (the fmf-service
    /// `setup` verb), returning its outcome.</param>
    /// <param name="relaunch">Unelevated relaunch of this app forcing the pipe
    /// transport, so the fresh instance binds the retrying pipe client.</param>
    internal ServiceProvisioner(
        Func<Task<ServiceActionOutcome>> register,
        Action relaunch)
    {
        _register = register;
        _relaunch = relaunch;
    }

    /// <summary>The production provisioner: the real elevated setup and
    /// <see cref="ShellOps.RelaunchIntoPipe"/>. Callers default to this.</summary>
    public static ServiceProvisioner Real { get; } = new(
        RegisterElevatedAsync,
        ShellOps.RelaunchIntoPipe);

    /// <summary>install (idempotent) + restart in one elevated step (the
    /// fmf-service `setup` verb), forwarding the daily user's SID so OTS
    /// elevation doesn't lock them out (docs/SECURITY.md threat 1). Blocking
    /// work runs off the UI thread.</summary>
    /// <returns>The outcome of the elevated setup step (success, declined, or failed).</returns>
    public Task<ServiceActionOutcome> RegisterAsync() => _register();

    /// <summary>Relaunch this app forcing the pipe transport so the fresh instance
    /// binds a retrying pipe client to the just-registered service. On success this
    /// process exits and never returns. Call only after <see cref="RegisterAsync"/>
    /// reports success — the relaunch assumes a service is now installed and starting.</summary>
    public void RelaunchIntoPipe() => _relaunch();

    /// <summary>The real elevated setup behind <see cref="Real"/>: locate
    /// fmf-service.exe, forward the SID-validated owner flag, and run the
    /// elevated `setup` verb off the UI thread.</summary>
    /// <returns>The outcome of the elevated setup, or Failed when the exe is missing.</returns>
    private static async Task<ServiceActionOutcome> RegisterElevatedAsync()
    {
        var exe = ServiceSetup.LocateServiceExe(AppContext.BaseDirectory);
        if (exe is null)
        {
            FileLog.Warn("service-ui", "fmf-service.exe not found — cannot register");
            return ServiceActionOutcome.Failed;
        }

        var sid = ServiceSetup.CurrentUserSid();
        var args = ServiceSetup.IsValidSid(sid) ? $"setup --owner-sid={sid}" : "setup";
        var result = await Task.Run(() => ServiceSetup.RunElevated(exe, args)).ConfigureAwait(false);
        FileLog.Info("service-ui", $"`{args}` → {result.Outcome} (exit {result.ExitCode})");
        return result.Outcome;
    }
}
