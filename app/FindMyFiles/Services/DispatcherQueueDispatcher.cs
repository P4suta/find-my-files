using Microsoft.UI.Dispatching;

namespace FindMyFiles.Services;

/// <summary>
/// Production <see cref="IDispatcher"/>: a thin wrapper over the UI thread's
/// cached <see cref="DispatcherQueue"/> (CLAUDE.md UI固定則 — cache on the UI
/// thread, TryEnqueue from background threads).
/// </summary>
public sealed class DispatcherQueueDispatcher(DispatcherQueue queue) : IDispatcher
{
    public bool HasThreadAccess => queue.HasThreadAccess;

    public bool TryEnqueue(Action action) => queue.TryEnqueue(() => action());

    public IDispatcherTimer CreateOneShotTimer(TimeSpan interval, Action tick)
    {
        var timer = queue.CreateTimer();
        timer.Interval = interval;
        timer.IsRepeating = false;
        timer.Tick += (_, _) => tick();
        return new OneShotTimer(timer);
    }

    private sealed class OneShotTimer(DispatcherQueueTimer timer) : IDispatcherTimer
    {
        public void Start()
        {
            timer.Stop(); // restart semantics: a pending tick is superseded
            timer.Start();
        }

        public void Stop() => timer.Stop();
    }
}
