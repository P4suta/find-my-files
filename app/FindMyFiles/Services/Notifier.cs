using System.Collections.Concurrent;

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

/// <summary>One entry in the InfoBar stack. Immutable, identity-stamped, and
/// carries an optional action button so a notification can offer its own
/// remedy (e.g. "restart the app" after a service install).</summary>
/// <param name="Severity">Visual style + log level + auto-dismiss policy.</param>
/// <param name="Message">The headline shown in the InfoBar.</param>
/// <param name="Detail">Optional secondary line (often an exception message).</param>
/// <param name="ActionLabel">Caption for the action button; null hides it.</param>
/// <param name="Action">Invoked when the action button is pressed; see
/// <see cref="Invoke"/>.</param>
public sealed record AppNotification(
    NotifySeverity Severity,
    string Message,
    string? Detail = null,
    string? ActionLabel = null,
    Action? Action = null)
{
    /// <summary>Stable per-notification identity (a hex GUID) so the InfoBar
    /// list can track and remove this exact entry.</summary>
    public string Id { get; } = Guid.NewGuid().ToString("N");

    /// <summary>x:Bind target for the InfoBar action button (no-op when the
    /// notification carries no action).</summary>
    public void Invoke() => Action?.Invoke();
}

/// <summary>
/// Process-wide notification funnel. Anything (global handlers, background
/// tasks, the engine callback) can post from any thread; the ViewModel
/// subscribes and marshals to the UI InfoBar stack. Posts are also mirrored
/// to the file log so nothing the user saw is missing from a bug report.
/// </summary>
public static class Notifier
{
    /// <summary>Raised on the posting thread for each notification once a
    /// subscriber exists. Subscribe via <see cref="Attach"/> (which also
    /// replays the pre-subscription backlog) rather than touching this
    /// directly; the ViewModel handler marshals to the UI thread.</summary>
    public static event Action<AppNotification>? Posted;

    /// <summary>Queue for posts that happen before the UI subscribes.</summary>
    private static readonly ConcurrentQueue<AppNotification> Pending = new();

    /// <summary>Post a notification from any thread. Mirrors it to the file log
    /// at the level matching <paramref name="severity"/> (so nothing the user
    /// saw is missing from a bug report), then hands it to the subscriber — or
    /// queues it until one attaches.</summary>
    /// <param name="severity">Style/level and auto-dismiss policy.</param>
    /// <param name="message">Headline text.</param>
    /// <param name="detail">Optional secondary line (e.g. exception message).</param>
    /// <param name="actionLabel">Caption for an optional action button.</param>
    /// <param name="action">Callback for the action button, if any.</param>
    public static void Post(
        NotifySeverity severity,
        string message,
        string? detail = null,
        string? actionLabel = null,
        Action? action = null)
    {
        var n = new AppNotification(severity, message, detail, actionLabel, action);
        switch (severity)
        {
            case NotifySeverity.Error:
                FileLog.Error("notify", $"{message} {detail}");
                break;
            case NotifySeverity.Warning:
                FileLog.Warn("notify", $"{message} {detail}");
                break;
            default:
                FileLog.Info("notify", message);
                break;
        }
        var handler = Posted;
        if (handler is null)
        {
            Pending.Enqueue(n);
        }
        else
        {
            handler(n);
        }
    }

    /// <summary>Drain posts that arrived before the UI was ready.</summary>
    public static void Attach(Action<AppNotification> handler)
    {
        Posted += handler;
        while (Pending.TryDequeue(out var n))
        {
            handler(n);
        }
    }
}
