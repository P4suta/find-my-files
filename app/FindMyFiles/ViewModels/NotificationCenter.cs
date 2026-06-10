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

    public ObservableCollection<AppNotification> Items { get; } = [];

    public NotificationCenter(IDispatcher dispatcher)
    {
        _dispatcher = dispatcher;
    }

    /// <summary>Drain the process-wide funnel into this stack.</summary>
    public void AttachToNotifier() =>
        Notifier.Attach(n => _dispatcher.TryEnqueue(() => Push(n)));

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

    public void Remove(AppNotification n) => Items.Remove(n);
}
