using System.Collections.Concurrent;

namespace FindMyFiles.Services;

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
    /// <param name="handler">Subscriber invoked for each notification, including
    /// the replayed pre-subscription backlog.</param>
    public static void Attach(Action<AppNotification> handler)
    {
        Posted += handler;
        while (Pending.TryDequeue(out var n))
        {
            handler(n);
        }
    }
}
