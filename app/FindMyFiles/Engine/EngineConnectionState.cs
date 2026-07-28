namespace FindMyFiles.Engine;

/// <summary>Transport state of the engine boundary. In-proc clients are
/// always InProc; the pipe client reports its supervisor state.</summary>
internal enum EngineConnectionState
{
    /// <summary>No engine is available. Setup or repair must create a new client.</summary>
    Unavailable,

    /// <summary>In-process engine (FFI): no transport, so always connected.</summary>
    InProc,

    /// <summary>The pipe client is establishing its first connection to the service.</summary>
    Connecting,

    /// <summary>The pipe client has a live connection to the service.</summary>
    Connected,

    /// <summary>The pipe connection dropped; the supervisor is re-establishing it.</summary>
    Reconnecting,

    /// <summary>The pipe failed a non-recoverable identity or protocol check.
    /// The supervisor is stopped; a new client must be created after the
    /// installation/version problem is repaired.</summary>
    Faulted,
}
