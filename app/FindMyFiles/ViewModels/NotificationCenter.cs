using System.Collections.ObjectModel;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// The InfoBar notification stack: capped at three, Info entries dissolve
/// after five seconds. Every error path in the app funnels through here via
/// <see cref="Notifier"/>. UI thread only (the Notifier subscription
/// marshals).
/// </summary>
internal sealed class NotificationCenter : IDisposable
{
    private const int MaxItems = 3;
    private static readonly TimeSpan InfoLifetime = TimeSpan.FromSeconds(5);

    private readonly IDispatcher _dispatcher;
    private readonly List<IDispatcherTimer> _timers = [];
    private IDisposable? _subscription;
    private bool _disposed;

    /// <summary>The live InfoBar stack (oldest first, capped at three),
    /// x:Bind'd by the view. Mutated on the UI thread only.</summary>
    public ObservableCollection<AppNotification> Items { get; } = [];

    /// <summary>Create the stack bound to <paramref name="dispatcher"/>, used
    /// to marshal posts onto the UI thread and to drive the Info auto-dismiss
    /// timer.</summary>
    /// <param name="dispatcher">UI-thread dispatch boundary.</param>
    public NotificationCenter(IDispatcher dispatcher)
    {
        _dispatcher = dispatcher;
    }

    /// <summary>Drain the process-wide funnel into this stack.</summary>
    public void AttachToNotifier()
    {
        if (_disposed || _subscription is not null)
        {
            return;
        }

        _subscription = Notifier.Attach(
            n => _dispatcher.TryEnqueue(() => Push(n)));
    }

    /// <summary>Append <paramref name="n"/> to the stack, evicting the oldest
    /// entries to stay within the three-item cap; Info entries also schedule
    /// their own five-second removal. UI thread only.</summary>
    /// <param name="n">The notification to show.</param>
    public void Push(AppNotification n)
    {
        if (_disposed)
        {
            return;
        }

        while (Items.Count >= MaxItems)
        {
            Items.RemoveAt(0);
        }

        Items.Add(n);
        if (n.Severity == NotifySeverity.Info)
        {
            var timer = _dispatcher.CreateOneShotTimer(InfoLifetime, () =>
            {
                if (!_disposed)
                {
                    Items.Remove(n);
                }
            });
            _timers.Add(timer);
            timer.Start();
        }
    }

    /// <summary>Remove <paramref name="n"/> from the stack — the InfoBar close
    /// button's target. No-op if it is already gone. UI thread only.</summary>
    /// <param name="n">The notification to dismiss.</param>
    public void Remove(AppNotification n) => Items.Remove(n);

    /// <summary>Detach the process-wide subscription and cancel every pending
    /// auto-dismiss timer. Idempotent; queued posts observe the disposed guard.</summary>
    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _subscription?.Dispose();
        _subscription = null;
        foreach (var timer in _timers)
        {
            timer.Stop();
        }

        _timers.Clear();
    }
}
