using FindMyFiles.Services;

namespace FindMyFiles.Tests.TestDoubles;

/// <summary>
/// An <see cref="IAppExit"/> that throws when invoked off a captured UI thread —
/// a deterministic, Application-free proxy for the <c>RPC_E_WRONG_THREAD</c> that
/// <c>Application.Current.Exit()</c> raises off the UI thread. Lets a test prove
/// the orphaned-window bug: a relaunch resumed on a pool thread calls Exit()
/// there, it throws, <c>ShellOps.Run</c> swallows it, and the old window never
/// closes.
/// </summary>
internal sealed class ThreadAffineExit : IAppExit
{
    private readonly int _uiThreadId;

    public ThreadAffineExit(int uiThreadId) => _uiThreadId = uiThreadId;

    /// <summary>Number of times <see cref="Exit"/> succeeded (ran on the UI thread).</summary>
    public int Exits { get; private set; }

    public void Exit()
    {
        if (Environment.CurrentManagedThreadId != _uiThreadId)
        {
            throw new InvalidOperationException(
                $"RPC_E_WRONG_THREAD proxy: Exit() ran on thread {Environment.CurrentManagedThreadId}, expected UI thread {_uiThreadId}.");
        }

        Exits++;
    }
}
