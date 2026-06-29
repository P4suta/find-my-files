namespace FindMyFiles.Engine;

/// <summary>Runtime info about the fmf-service process behind the pipe
/// (mirrors fmf-proto's <c>ServiceInfoResp</c>, op 12). Pipe-only: null for
/// in-proc clients (Ffi/Fake) where there is no separate service. The pipe
/// client fetches it best-effort alongside the stats snapshot.</summary>
public sealed class ServiceInfoData
{
    /// <summary>How long the service process has been running, in ms.</summary>
    public ulong UptimeMs { get; set; }

    /// <summary>Active client connections the service is currently serving.</summary>
    public uint Connections { get; set; }

    /// <summary>Service binary version — the channel-aware
    /// <c>fmf_buildstamp::VERSION</c> (e.g. <c>0.1.0-nightly.&lt;date&gt;+g&lt;sha&gt;</c>),
    /// the same identity <c>fmf-service --version</c> reports.</summary>
    public string Version { get; set; } = string.Empty;
}
