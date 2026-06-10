using System.Collections.Concurrent;

namespace FindMyFiles.Services;

public enum NotifySeverity
{
    Info,
    Warning,
    Error,
}

public sealed record AppNotification(NotifySeverity Severity, string Message, string? Detail = null)
{
    public string Id { get; } = Guid.NewGuid().ToString("N");
}

/// <summary>
/// Process-wide notification funnel. Anything (global handlers, background
/// tasks, the engine callback) can post from any thread; the ViewModel
/// subscribes and marshals to the UI InfoBar stack. Posts are also mirrored
/// to the file log so nothing the user saw is missing from a bug report.
/// </summary>
public static class Notifier
{
    public static event Action<AppNotification>? Posted;

    /// <summary>Queue for posts that happen before the UI subscribes.</summary>
    private static readonly ConcurrentQueue<AppNotification> Pending = new();

    public static void Post(NotifySeverity severity, string message, string? detail = null)
    {
        var n = new AppNotification(severity, message, detail);
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
