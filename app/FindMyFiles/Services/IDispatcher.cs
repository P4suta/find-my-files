namespace FindMyFiles.Services;

/// <summary>
/// UI-thread dispatch boundary. The single seam that lets the search pipeline
/// and the virtualized list run under unit tests without a real
/// DispatcherQueue — two implementations by necessity:
/// <see cref="DispatcherQueueDispatcher"/> (production) and a manual fake in
/// the test project.
/// </summary>
public interface IDispatcher
{
    /// <summary>True when the caller is already on the UI thread.</summary>
    bool HasThreadAccess { get; }

    /// <summary>Queue <paramref name="action"/> to run on the UI thread.
    /// Returns false if the queue is shutting down and the work was not
    /// accepted.</summary>
    /// <param name="action">Work to marshal onto the UI thread.</param>
    /// <returns>True if enqueued; false if the dispatcher is no longer
    /// accepting work.</returns>
    bool TryEnqueue(Action action);

    /// <summary>
    /// One-shot timer: fires <paramref name="tick"/> once per Start().
    /// Start() while pending restarts the interval (debounce semantics).
    /// </summary>
    IDispatcherTimer CreateOneShotTimer(TimeSpan interval, Action tick);
}

/// <summary>The one-shot timer handle returned by
/// <see cref="IDispatcher.CreateOneShotTimer"/> — fires on the UI thread and is
/// restartable for debounce.</summary>
public interface IDispatcherTimer
{
    /// <summary>Arm the timer. Calling while a tick is still pending restarts
    /// the interval (debounce semantics).</summary>
    void Start();

    /// <summary>Cancel a pending tick, if any. Idempotent.</summary>
    [System.Diagnostics.CodeAnalysis.SuppressMessage(
        "Naming",
        "CA1716:Identifiers should not match keywords",
        Justification = "internal abstraction mirroring WinUI DispatcherTimer.Stop(); C#-only app, no cross-language consumers")]
    void Stop();
}
