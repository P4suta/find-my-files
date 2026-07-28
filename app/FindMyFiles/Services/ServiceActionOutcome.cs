namespace FindMyFiles.Services;

/// <summary>Verdict of a lifecycle-action preflight or the elevated action
/// itself (<see cref="ServiceSetup.RunElevated"/>). Output is unreadable under
/// ShellExecute, so the exit code is the only post-launch signal; preflight and
/// a declined UAC prompt remain distinct so the UI can explain the remedy.</summary>
internal enum ServiceActionOutcome
{
    /// <summary>The elevated action exited 0 — the verb succeeded.</summary>
    Ok,

    /// <summary>The action ran but exited non-zero (or could not be
    /// launched/timed out) — a genuine failure to surface to the user.</summary>
    Failed,

    /// <summary>The user dismissed the UAC prompt (ERROR_CANCELLED 1223) — not
    /// a failure, so the UI says "cancelled" rather than "error".</summary>
    Cancelled,

    /// <summary>The daily user's SID could not be captured and validated, so
    /// setup stopped before UAC rather than installing an owner-less service.</summary>
    IdentityUnavailable,
}
