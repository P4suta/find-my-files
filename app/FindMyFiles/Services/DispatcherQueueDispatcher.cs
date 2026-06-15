using Microsoft.UI.Dispatching;

namespace FindMyFiles.Services;

/// <summary>
/// Production <see cref="IDispatcher"/>: a thin wrapper over the UI thread's
/// cached <see cref="DispatcherQueue"/> (CLAUDE.md UI固定則 — cache on the UI
/// thread, TryEnqueue from background threads).
/// </summary>
/// <param name="queue">The UI thread's <see cref="DispatcherQueue"/>, captured
/// once on that thread at construction.</param>
// UI-thread DispatcherQueue wrapper: exercised only on a live UI thread; the
// ManualDispatcher fake stands in for tests (ADR-0022).
[System.Diagnostics.CodeAnalysis.ExcludeFromCodeCoverage]
public sealed class DispatcherQueueDispatcher(DispatcherQueue queue) : IDispatcher
{
    /// <inheritdoc/>
    public bool HasThreadAccess => queue.HasThreadAccess;

    /// <inheritdoc/>
    public bool TryEnqueue(Action action) => queue.TryEnqueue(() => action());

    /// <inheritdoc/>
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
