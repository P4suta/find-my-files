using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>
/// The shared "make file search usable" steps, behind both the setup screen's
/// one-click button and the management dialog's register action: register the
/// fmf-engine service elevated, then wait for its pipe to start serving and
/// relaunch so this (unelevated, empty-fake) instance comes back connected.
/// The engine transport is chosen once at startup, so a relaunch is how a
/// freshly registered service takes effect.
/// </summary>
public static class ServiceProvisioner
{
    /// <summary>install (idempotent) + restart in one elevated step (the
    /// fmf-service `setup` verb), forwarding the daily user's SID so OTS
    /// elevation doesn't lock them out (docs/SECURITY.md 脅威1). Blocking work
    /// runs off the UI thread.</summary>
    public static async Task<ServiceActionOutcome> RegisterAsync()
    {
        var exe = ServiceSetup.LocateServiceExe(AppContext.BaseDirectory);
        if (exe is null)
        {
            FileLog.Warn("service-ui", "fmf-service.exe not found — cannot register");
            return ServiceActionOutcome.Failed;
        }
        var sid = ServiceSetup.CurrentUserSid();
        var args = ServiceSetup.IsValidSid(sid) ? $"setup --owner-sid={sid}" : "setup";
        var result = await Task.Run(() => ServiceSetup.RunElevated(exe, args));
        FileLog.Info("service-ui", $"`{args}` → {result.Outcome} (exit {result.ExitCode})");
        return result.Outcome;
    }

    /// <summary>After register/start the service's pipe needs a moment to start
    /// serving — poll until it answers (≈8s budget), then relaunch this app so
    /// the fresh instance connects. Returns false if the pipe never came up in
    /// time (the caller then offers a manual retry). On success this process
    /// exits and never returns.</summary>
    public static async Task<bool> WaitForServiceThenRelaunchAsync()
    {
        for (var attempt = 0; attempt < 16; attempt++)
        {
            if (await PipeEngineClient.ProbeAsync(
                    PipeProtocol.DefaultPipeName, TimeSpan.FromMilliseconds(300)))
            {
                ShellOps.Relaunch(); // replaces this process with a connected one
                return true;
            }
            await Task.Delay(200);
        }
        return false;
    }
}
