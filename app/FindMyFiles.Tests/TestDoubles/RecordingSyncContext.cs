namespace FindMyFiles.Tests.TestDoubles;

/// <summary>A deferred <see cref="SynchronizationContext"/> for UI-thread-affinity
/// tests: it records how many continuations were marshaled to it (<see cref="Posted"/>)
/// and runs them only when the test pumps (<see cref="Drain"/>) — the message-loop
/// behavior a real UI dispatcher provides, made deterministic. Install it as
/// <see cref="SynchronizationContext.Current"/>, let a production <c>await</c> (without
/// <c>ConfigureAwait(false)</c>) capture it, then assert the continuation was Posted
/// here rather than run inline on whatever thread completed the awaited task.</summary>
public sealed class RecordingSyncContext : SynchronizationContext
{
    private readonly Queue<(SendOrPostCallback Callback, object? State)> _queue = new();

    /// <summary>Continuations marshaled here via <see cref="Post"/>.</summary>
    public int Posted { get; private set; }

    public override void Post(SendOrPostCallback d, object? state)
    {
        Posted++;
        _queue.Enqueue((d, state));
    }

    /// <summary>Pump every queued continuation with this context installed as
    /// Current — the message-loop behavior a real UI dispatcher provides.</summary>
    public void Drain()
    {
        var previous = Current;
        SetSynchronizationContext(this);
        try
        {
            while (_queue.Count > 0)
            {
                var (callback, state) = _queue.Dequeue();
                callback(state);
            }
        }
        finally
        {
            SetSynchronizationContext(previous);
        }
    }
}
