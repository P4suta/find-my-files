using System.Collections.ObjectModel;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// The InfoBar notification stack: capped at three, Info entries dissolve
/// after five seconds. Every error path in the app funnels through here via
/// <see cref="Notifier"/>. UI thread only (the Notifier subscription
/// marshals).
/// </summary>
public sealed class NotificationCenter
{
    private const int MaxItems = 3;
    private static readonly TimeSpan InfoLifetime = TimeSpan.FromSeconds(5);

    private readonly IDispatcher _dispatcher;

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
    public void AttachToNotifier() =>
        Notifier.Attach(n => _dispatcher.TryEnqueue(() => Push(n)));

    /// <summary>Append <paramref name="n"/> to the stack, evicting the oldest
    /// entries to stay within the three-item cap; Info entries also schedule
    /// their own five-second removal. UI thread only.</summary>
    /// <param name="n">The notification to show.</param>
    public void Push(AppNotification n)
    {
        while (Items.Count >= MaxItems)
        {
            Items.RemoveAt(0);
        }
        Items.Add(n);
        if (n.Severity == NotifySeverity.Info)
        {
            _dispatcher.CreateOneShotTimer(InfoLifetime, () => Items.Remove(n)).Start();
        }
    }

    /// <summary>Remove <paramref name="n"/> from the stack — the InfoBar close
    /// button's target. No-op if it is already gone. UI thread only.</summary>
    /// <param name="n">The notification to dismiss.</param>
    public void Remove(AppNotification n) => Items.Remove(n);
}
