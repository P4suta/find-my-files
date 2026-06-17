using System.Collections.Concurrent;
using FindMyFiles.Services;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// An <see cref="IDispatcher"/> backed by a REAL dedicated thread with its own
/// <see cref="SynchronizationContext"/> and message pump — the counterpart to
/// <see cref="ManualDispatcher"/> for tests that must observe thread identity.
/// Unlike ManualDispatcher (whose <c>HasThreadAccess</c> is always true), this
/// reports access only on its pump thread, so a continuation that resumes off
/// the UI thread — a stray <c>ConfigureAwait(false)</c> on a UI-affine path —
/// is detectable. That is the bug class that ships RPC_E_WRONG_THREAD crashes
/// (the orphaned-window setup bug), which ManualDispatcher structurally can't see.
/// </summary>
public sealed class DedicatedThreadDispatcher : IDispatcher, IDisposable
{
    private readonly BlockingCollection<Action> _queue = new();
    private readonly Thread _thread;
    private volatile bool _shuttingDown;

    public DedicatedThreadDispatcher()
    {
        using var ready = new ManualResetEventSlim();
        _thread = new Thread(() => Pump(ready))
        {
            IsBackground = true,
            Name = "DedicatedThreadDispatcher",
        };
        _thread.Start();
        ready.Wait();
        ThreadId = _thread.ManagedThreadId;
    }

    /// <summary>Managed id of the pump thread — the "UI thread" for tests.</summary>
    public int ThreadId { get; }

    /// <summary>First unhandled exception from a posted action, if any (posted
    /// continuations normally route their own failures elsewhere).</summary>
    public Exception? LastError { get; private set; }

    public bool HasThreadAccess => Environment.CurrentManagedThreadId == ThreadId;

    public bool TryEnqueue(Action action)
    {
        if (_shuttingDown)
        {
            return false;
        }

        try
        {
            _queue.Add(action);
            return true;
        }
        catch (InvalidOperationException)
        {
            return false; // CompleteAdding raced with us
        }
    }

    public IDispatcherTimer CreateOneShotTimer(TimeSpan interval, Action tick) =>
        new PumpTimer(this, interval, tick);

    /// <summary>Run <paramref name="work"/> on the pump thread (so its awaits
    /// capture this dispatcher's context) and complete when it finishes.</summary>
    /// <param name="work">The async work to run on the pump thread.</param>
    /// <returns>A task that completes when <paramref name="work"/> finishes.</returns>
    public Task InvokeAsync(Func<Task> work) =>
        InvokeAsync(async () =>
        {
            await work();
            return true;
        });

    /// <summary>Run <paramref name="work"/> on the pump thread and return its result.</summary>
    /// <typeparam name="T">The result type.</typeparam>
    /// <param name="work">The async work to run on the pump thread.</param>
    /// <returns>A task carrying the result of <paramref name="work"/>.</returns>
    public Task<T> InvokeAsync<T>(Func<Task<T>> work)
    {
        var tcs = new TaskCompletionSource<T>();
        if (!TryEnqueue(() => Bridge(work, tcs)))
        {
            tcs.SetException(new InvalidOperationException("dispatcher is shutting down"));
        }

        return tcs.Task;
    }

    public void Dispose()
    {
        _shuttingDown = true;
        _queue.CompleteAdding();
        if (Environment.CurrentManagedThreadId != ThreadId)
        {
            _thread.Join(TimeSpan.FromSeconds(5));
        }

        _queue.Dispose();
        GC.SuppressFinalize(this);
    }

    private static async void Bridge<T>(Func<Task<T>> work, TaskCompletionSource<T> tcs)
    {
        try
        {
            tcs.SetResult(await work());
        }
        catch (Exception ex)
        {
            tcs.SetException(ex);
        }
    }

    private void Pump(ManualResetEventSlim ready)
    {
        SynchronizationContext.SetSynchronizationContext(new PumpSyncContext(this));
        ready.Set();
        foreach (var action in _queue.GetConsumingEnumerable())
        {
            try
            {
                action();
            }
            catch (Exception ex)
            {
                LastError ??= ex;
            }
        }
    }

    /// <summary>Routes async continuations Posted to the captured context back
    /// onto the pump thread — what makes <c>await</c> resume on the "UI thread".</summary>
    private sealed class PumpSyncContext : SynchronizationContext
    {
        private readonly DedicatedThreadDispatcher _owner;

        public PumpSyncContext(DedicatedThreadDispatcher owner) => _owner = owner;

        public override void Post(SendOrPostCallback d, object? state) =>
            _owner.TryEnqueue(() => d(state));

        public override void Send(SendOrPostCallback d, object? state)
        {
            if (_owner.HasThreadAccess)
            {
                d(state);
                return;
            }

            using var done = new ManualResetEventSlim();
            Exception? error = null;
            _owner.TryEnqueue(() =>
            {
                try
                {
                    d(state);
                }
                catch (Exception ex)
                {
                    error = ex;
                }
                finally
                {
                    done.Set();
                }
            });
            done.Wait();
            if (error is not null)
            {
                throw error;
            }
        }

        public override SynchronizationContext CreateCopy() => this;
    }

    /// <summary>One-shot timer whose tick fires on the pump thread (like a real
    /// DispatcherQueueTimer). Generation-guarded so a restart cancels the pending
    /// tick (debounce). No disposable field, so the handle stays non-disposable.</summary>
    private sealed class PumpTimer : IDispatcherTimer
    {
        private readonly DedicatedThreadDispatcher _owner;
        private readonly TimeSpan _interval;
        private readonly Action _tick;
        private int _generation;
        private volatile bool _armed;

        public PumpTimer(DedicatedThreadDispatcher owner, TimeSpan interval, Action tick)
        {
            _owner = owner;
            _interval = interval;
            _tick = tick;
        }

        public void Start()
        {
            _armed = true;
            var generation = Interlocked.Increment(ref _generation);
            DelayThenTick(generation).Forget("ddt-timer");
        }

        public void Stop()
        {
            _armed = false;
            Interlocked.Increment(ref _generation);
        }

        private async Task DelayThenTick(int generation)
        {
            await Task.Delay(_interval).ConfigureAwait(false);
            if (_armed && Volatile.Read(ref _generation) == generation)
            {
                _owner.TryEnqueue(() =>
                {
                    if (_armed)
                    {
                        _armed = false;
                        _tick();
                    }
                });
            }
        }
    }
}
