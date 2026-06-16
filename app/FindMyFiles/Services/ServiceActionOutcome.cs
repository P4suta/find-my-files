namespace FindMyFiles.Services;

/// <summary>Verdict of one elevated lifecycle action (<see
/// cref="ServiceSetup.RunElevated"/>). Output is unreadable under
/// ShellExecute, so the exit code is the only signal; a declined UAC prompt
/// is distinguished from a genuine failure so the UI can say so.</summary>
public enum ServiceActionOutcome
{
    /// <summary>The elevated action exited 0 — the verb succeeded.</summary>
    Ok,

    /// <summary>The action ran but exited non-zero (or could not be
    /// launched/timed out) — a genuine failure to surface to the user.</summary>
    Failed,

    /// <summary>The user dismissed the UAC prompt (ERROR_CANCELLED 1223) — not
    /// a failure, so the UI says "cancelled" rather than "error".</summary>
    Cancelled,
}
