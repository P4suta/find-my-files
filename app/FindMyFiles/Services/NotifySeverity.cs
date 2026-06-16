namespace FindMyFiles.Services;

/// <summary>Severity of an <see cref="AppNotification"/> — selects the InfoBar
/// style and the file-log level, and decides whether the entry auto-dismisses
/// (Info) or stays until the user closes it.</summary>
public enum NotifySeverity
{
    /// <summary>Transient confirmation — logged at INFO; dissolves on its own.</summary>
    Info,

    /// <summary>A degraded path the user should know about — logged at WARN;
    /// stays until dismissed.</summary>
    Warning,

    /// <summary>A failure — logged at ERROR; stays until dismissed.</summary>
    Error,
}
