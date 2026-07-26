using System.Collections.Concurrent;

namespace FindMyFiles.Services;

/// <summary>
/// Process-wide notification funnel. Anything (global handlers, background
/// tasks, the engine callback) can post from any thread; the ViewModel
/// subscribes and marshals to the UI InfoBar stack. Posts are also mirrored
/// to the file log so nothing the user saw is missing from a bug report.
/// </summary>
internal static class Notifier
{
    private static readonly System.Threading.Lock Gate = new();
    private static Action<AppNotification>? _posted;

    /// <summary>Raised on the posting thread for each notification once a
    /// subscriber exists. Subscribe via <see cref="Attach"/> (which also
    /// replays the pre-subscription backlog) rather than touching this
    /// directly; the ViewModel handler marshals to the UI thread.</summary>
    public static event Action<AppNotification>? Posted
    {
        add
        {
            lock (Gate)
            {
                _posted += value;
            }
        }

        remove
        {
            lock (Gate)
            {
                _posted -= value;
            }
        }
    }

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
                FileLog.ErrorEvent(
                    "notify",
                    "notification posted",
                    fields:
                    [
                        ("message_len", message.Length),
                        ("detail_len", detail?.Length ?? 0),
                    ]);
                break;
            case NotifySeverity.Warning:
                FileLog.WarnEvent(
                    "notify",
                    "notification posted",
                    fields:
                    [
                        ("message_len", message.Length),
                        ("detail_len", detail?.Length ?? 0),
                    ]);
                break;
            default:
                FileLog.Event(
                    "notify",
                    "notification posted",
                    ("message_len", message.Length),
                    ("detail_len", detail?.Length ?? 0));
                break;
        }

        Action<AppNotification>? handlers;
        lock (Gate)
        {
            handlers = _posted;
            if (handlers is null)
            {
                Pending.Enqueue(n);
                return;
            }
        }

        InvokeSafely(handlers, n);
    }

    private static void InvokeSafely(
        Action<AppNotification> handlers,
        AppNotification notification)
    {
        foreach (Action<AppNotification> handler in handlers.GetInvocationList())
        {
            try
            {
                handler(notification);
            }
            catch (Exception ex)
            {
                try
                {
                    FileLog.Error("notify", "notification subscriber failed", ex);
                }
                catch
                {
                    // A diagnostics failure must never escape the global
                    // notification boundary either.
                }
            }
        }
    }

    /// <summary>Attach one subscriber and drain posts that arrived before the
    /// UI was ready. The returned token removes exactly this subscription.</summary>
    /// <param name="handler">Subscriber invoked for each notification, including
    /// the replayed pre-subscription backlog.</param>
    /// <returns>An idempotent subscription token.</returns>
    public static IDisposable Attach(Action<AppNotification> handler)
    {
        ArgumentNullException.ThrowIfNull(handler);
        lock (Gate)
        {
            _posted += handler;
            while (Pending.TryDequeue(out var n))
            {
                InvokeSafely(handler, n);
            }
        }

        return new Subscription(handler);
    }

#if FMF_TEST_SEAMS
    /// <summary>Test-only: drop every subscriber and the pending backlog so this
    /// process-wide static cannot leak posts across tests — a post made with no
    /// live subscriber is queued and replayed into the next one to attach. Never
    /// for production, where the app keeps a single long-lived subscriber.</summary>
    internal static void ResetForTests()
    {
        lock (Gate)
        {
            _posted = null;
            while (Pending.TryDequeue(out _))
            {
            }
        }
    }
#endif

    private sealed class Subscription(Action<AppNotification> handler) : IDisposable
    {
        private Action<AppNotification>? _handler = handler;

        public void Dispose()
        {
            var current = Interlocked.Exchange(ref _handler, null);
            if (current is null)
            {
                return;
            }

            lock (Gate)
            {
                _posted -= current;
            }
        }
    }
}
