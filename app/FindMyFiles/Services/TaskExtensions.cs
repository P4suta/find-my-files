namespace FindMyFiles.Services;

/// <summary>Extensions for safely launching fire-and-forget <see cref="Task"/>s
/// (CLAUDE.md規約: never <c>_ = SomeAsync()</c>).</summary>
public static class TaskExtensions
{
    /// <summary>
    /// The only sanctioned way to fire-and-forget (CLAUDE.md規約): unexpected
    /// exceptions land in the log and the notification bar instead of being
    /// silently dropped by an abandoned Task.
    /// </summary>
    /// <param name="task">The task to observe to completion.</param>
    /// <param name="area">Subsystem tag for the log/notification if it faults.</param>
    public static async void Forget(this Task task, string area)
    {
        try
        {
            await task.ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            FileLog.Error(area, "background task failed", ex);
            Notifier.Post(
                NotifySeverity.Error,
                Loc.Get("Crash_InternalArea", area),
                ex.Message);
        }
    }
}
