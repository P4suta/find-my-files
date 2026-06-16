namespace FindMyFiles.Engine;

/// <summary>One entry from the engine's diagnostic ring (WARN+ events and
/// panics; mirrors fmf-core's <c>ErrorEvent</c>). Pulled on demand after an
/// <see cref="IEngineClient.EngineErrorOccurred"/> signal and listed in the
/// F12 panel.</summary>
public sealed class ErrorEventData
{
    /// <summary>Monotonic emit sequence — orders events and lets the UI
    /// detect ones it has already shown.</summary>
    public ulong Seq { get; set; }

    /// <summary>Engine uptime in ms when the event fired ("when").</summary>
    public ulong UptimeMs { get; set; }

    /// <summary>Level as a lowercase string: <c>"warn"</c>, <c>"error"</c> or
    /// <c>"panic"</c> (the same 1/2/3 the FFI event carries numerically).</summary>
    public string Severity { get; set; } = string.Empty; // warn|error|panic

    /// <summary>Originating <c>tracing</c> target (module path) — the "where"
    /// of the event.</summary>
    public string Area { get; set; } = string.Empty;

    /// <summary>Drive label the event is attributed to, or null when it is
    /// not volume-scoped.</summary>
    public string? Volume { get; set; }

    /// <summary>Human-readable description of what happened.</summary>
    public string Message { get; set; } = string.Empty;
}
