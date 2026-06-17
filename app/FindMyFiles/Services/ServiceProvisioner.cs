using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>
/// The shared "make file search usable" steps, behind both the setup screen's
/// one-click button and the management dialog's register action: register the
/// fmf-engine service elevated, then wait for its pipe to start serving and
/// relaunch so this (unelevated, empty-fake) instance comes back connected.
/// The engine transport is chosen once at startup, so a relaunch is how a
/// freshly registered service takes effect.
/// <para>Instance-based so the elevated setup, the pipe probe, the relaunch and
/// the inter-probe delay are injectable boundaries (ADR-0022) — production code
/// uses <see cref="Real"/>, which wires the real statics; tests drive fakes.</para>
/// </summary>
public sealed class ServiceProvisioner
{
    private readonly Func<Task<ServiceActionOutcome>> _register;
    private readonly Func<string, TimeSpan, Task<bool>> _probe;
    private readonly Action _relaunch;
    private readonly Func<TimeSpan, Task> _delay;

    /// <summary>Builds a provisioner over its four boundaries. Internal so only
    /// production (<see cref="Real"/>) and tests construct it; callers receive
    /// one by injection, defaulting to <see cref="Real"/>.</summary>
    /// <param name="register">The elevated install+start step (the fmf-service
    /// `setup` verb), returning its outcome.</param>
    /// <param name="probe">Pipe probe: does the service answer a Hello within the timeout?</param>
    /// <param name="relaunch">Unelevated relaunch of this app so the fresh
    /// instance picks up the now-running service over the pipe.</param>
    /// <param name="delay">Inter-probe backoff (injectable so tests don't wait ≈8s).</param>
    internal ServiceProvisioner(
        Func<Task<ServiceActionOutcome>> register,
        Func<string, TimeSpan, Task<bool>> probe,
        Action relaunch,
        Func<TimeSpan, Task> delay)
    {
        _register = register;
        _probe = probe;
        _relaunch = relaunch;
        _delay = delay;
    }

    /// <summary>The production provisioner: the real elevated setup, the real
    /// pipe probe, <see cref="ShellOps.Relaunch"/>, and
    /// <see cref="Task.Delay(TimeSpan)"/>. Callers default to this.</summary>
    public static ServiceProvisioner Real { get; } = new(
        RegisterElevatedAsync,
        (name, timeout) => PipeEngineClient.ProbeAsync(name, timeout),
        ShellOps.Relaunch,
        d => Task.Delay(d));

    /// <summary>install (idempotent) + restart in one elevated step (the
    /// fmf-service `setup` verb), forwarding the daily user's SID so OTS
    /// elevation doesn't lock them out (docs/SECURITY.md threat 1). Blocking
    /// work runs off the UI thread.</summary>
    /// <returns>The outcome of the elevated setup step (success, declined, or failed).</returns>
    public Task<ServiceActionOutcome> RegisterAsync() => _register();

    /// <summary>After register/start the service's pipe needs a moment to start
    /// serving — poll until it answers (≈8s budget), then relaunch this app so
    /// the fresh instance connects. Returns false if the pipe never came up in
    /// time (the caller then offers a manual retry). On success this process
    /// exits and never returns.</summary>
    /// <returns>True once the pipe answered and a relaunch was triggered; false if it never came up in time.</returns>
    public async Task<bool> WaitForServiceThenRelaunchAsync()
    {
        for (var attempt = 0; attempt < 16; attempt++)
        {
            if (await _probe(PipeProtocol.DefaultPipeName, TimeSpan.FromMilliseconds(300))
                    .ConfigureAwait(false))
            {
                _relaunch(); // replaces this process with a connected one
                return true;
            }

            await _delay(TimeSpan.FromMilliseconds(200)).ConfigureAwait(false);
        }

        return false;
    }

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
