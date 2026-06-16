namespace FindMyFiles.Services;

/// <summary>SCM registration/run state of the fmf-engine service, as seen by
/// the unelevated UI via <see cref="ServiceSetup.QueryState"/> — drives whether
/// the app offers to install, start, or nothing at all.</summary>
public enum EngineServiceState
{
    /// <summary>No <see cref="FindMyFiles.Engine.EngineContract.ServiceName"/> entry in the SCM
    /// (or the SCM is unreachable) — the UI offers a one-time install.</summary>
    NotInstalled,

    /// <summary>Registered but not running — the UI offers to start it.</summary>
    Stopped,

    /// <summary>Running (or on its way up: START/CONTINUE_PENDING) — no offer
    /// needed; the pipe transport can connect.</summary>
    Running,
}
