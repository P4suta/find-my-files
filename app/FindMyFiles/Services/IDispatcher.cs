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

    bool TryEnqueue(Action action);

    /// <summary>
    /// One-shot timer: fires <paramref name="tick"/> once per Start().
    /// Start() while pending restarts the interval (debounce semantics).
    /// </summary>
    IDispatcherTimer CreateOneShotTimer(TimeSpan interval, Action tick);
}

public interface IDispatcherTimer
{
    void Start();
    void Stop();
}
