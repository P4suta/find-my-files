namespace FindMyFiles.Tests.TestDoubles;

internal static class SyncContext
{
    /// <summary>
    /// Run the rest of the test without a SynchronizationContext. xunit
    /// installs one that (a) tracks async-void completions — a deliberately
    /// never-completed stub SearchAsync would stall the runner forever at
    /// teardown via the production Forget() chain — and (b) posts await
    /// continuations to worker threads, making asserts placed right after a
    /// TaskCompletionSource.SetResult racy. With a null context every
    /// continuation runs inline on the test thread, so the whole pipeline
    /// executes synchronously and deterministically. Call first thing in any
    /// test that lets a production await suspend on a stub-controlled task.
    /// </summary>
    public static void RunContinuationsInline() =>
        SynchronizationContext.SetSynchronizationContext(null);
}
