namespace FindMyFiles.Services;

/// <summary>SCM registration/run state of the fmf-engine service, as seen by
/// the unelevated UI via <see cref="ServiceSetup.QueryState"/> — drives whether
/// the app offers to install, start, or nothing at all.</summary>
internal enum EngineServiceState
{
    /// <summary>No <see cref="FindMyFiles.Engine.EngineContract.ServiceName"/>
    /// entry exists in the SCM — the UI offers a one-time install.</summary>
    NotInstalled,

    /// <summary>Registered and definitively <c>SERVICE_STOPPED</c> — the UI may
    /// safely start it.</summary>
    Stopped,

    /// <summary>Any registered non-stopped lifecycle state. This includes
    /// start/stop/pause transitions: the service may still own the writer lock,
    /// so only the pipe path is safe.</summary>
    Running,

    /// <summary>The SCM or service state could not be read. Fail closed: probe
    /// the pipe, but never assume the in-proc writer lock is free.</summary>
    Unknown,
}
