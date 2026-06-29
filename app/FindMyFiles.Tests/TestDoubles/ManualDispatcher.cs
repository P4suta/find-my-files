using FindMyFiles.Services;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// Deterministic <see cref="IDispatcher"/> for unit tests: HasThreadAccess is
/// always true (the production Debug.Asserts must pass), TryEnqueue collects
/// work until the test drains it, and one-shot timers fire only when the test
/// says so. No real threads, no real time.
///
/// <para>The queue is locked because the real <c>DispatcherQueue.TryEnqueue</c>
/// is thread-safe and production relies on that: a background page fetch awaits
/// with <c>ConfigureAwait(false)</c> and marshals its completion back through
/// <c>TryEnqueue</c> — that continuation can resume on a thread-pool thread
/// (the TPL is free not to inline it) and race a test-thread
/// <see cref="DrainQueue"/>. An unsynchronized <see cref="Queue{T}"/> tears
/// under that concurrent enqueue/dequeue (a null slot → NRE). Actions are
/// invoked outside the lock so re-entrant enqueues and concurrent producers
/// never block on the action.</para>
/// </summary>
public sealed class ManualDispatcher : IDispatcher
{
    private readonly Queue<Action> _queue = new();
    private readonly System.Threading.Lock _queueLock = new();

    /// <summary>Every timer ever created, in creation order.</summary>
    public List<ManualTimer> Timers { get; } = [];

    public bool HasThreadAccess => true;

    public bool TryEnqueue(Action action)
    {
        lock (_queueLock)
        {
            _queue.Enqueue(action);
        }

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
        while (true)
        {
            Action next;
            lock (_queueLock)
            {
                if (_queue.Count == 0)
                {
                    break;
                }

                next = _queue.Dequeue();
            }

            next(); // outside the lock: an action may re-enqueue or a producer may run
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
