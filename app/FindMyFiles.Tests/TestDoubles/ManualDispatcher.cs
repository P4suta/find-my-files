using FindMyFiles.Services;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// Deterministic <see cref="IDispatcher"/> for unit tests: HasThreadAccess is
/// always true (the production Debug.Asserts must pass), TryEnqueue collects
/// work until the test drains it, and one-shot timers fire only when the test
/// says so. No real threads, no real time.
/// </summary>
public sealed class ManualDispatcher : IDispatcher
{
    private readonly Queue<Action> _queue = new();

    /// <summary>Every timer ever created, in creation order.</summary>
    public List<ManualTimer> Timers { get; } = [];

    public bool HasThreadAccess => true;

    public bool TryEnqueue(Action action)
    {
        _queue.Enqueue(action);
        return true;
    }

    public IDispatcherTimer CreateOneShotTimer(TimeSpan interval, Action tick)
    {
        var timer = new ManualTimer(interval, tick);
        Timers.Add(timer);
        return timer;
    }

    /// <summary>
    /// Run everything queued so far, including work the drained actions
    /// enqueue themselves. Returns the number of actions run.
    /// </summary>
    public int DrainQueue()
    {
        var ran = 0;
        while (_queue.Count > 0)
        {
            _queue.Dequeue()();
            ran++;
        }
        return ran;
    }

    /// <summary>Fire every currently-armed timer once (one-shot semantics).</summary>
    public void FireTimers()
    {
        // Snapshot: a tick may create or re-arm timers.
        foreach (var timer in Timers.Where(t => t.IsStarted).ToList())
        {
            timer.Fire();
        }
    }

    public sealed class ManualTimer(TimeSpan interval, Action tick) : IDispatcherTimer
    {
        public TimeSpan Interval { get; } = interval;

        /// <summary>Armed and waiting for <see cref="Fire"/>.</summary>
        public bool IsStarted { get; private set; }

        /// <summary>Start() calls so far — debounce restarts included.</summary>
        public int StartCount { get; private set; }

        public void Start()
        {
            StartCount++;
            IsStarted = true;
        }

        public void Stop() => IsStarted = false;

        /// <summary>Simulate the interval elapsing. No-op unless armed.</summary>
        public void Fire()
        {
            if (!IsStarted)
            {
                return;
            }
            IsStarted = false; // one-shot
            tick();
        }
    }
}
