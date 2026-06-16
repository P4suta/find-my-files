namespace FindMyFiles.Services;

/// <summary>One entry in the InfoBar stack. Immutable, identity-stamped, and
/// carries an optional action button so a notification can offer its own
/// remedy (e.g. "restart the app" after a service install).</summary>
/// <param name="Severity">Visual style + log level + auto-dismiss policy.</param>
/// <param name="Message">The headline shown in the InfoBar.</param>
/// <param name="Detail">Optional secondary line (often an exception message).</param>
/// <param name="ActionLabel">Caption for the action button; null hides it.</param>
/// <param name="Action">Invoked when the action button is pressed; see
/// <see cref="Invoke"/>.</param>
public sealed record AppNotification(
    NotifySeverity Severity,
    string Message,
    string? Detail = null,
    string? ActionLabel = null,
    Action? Action = null)
{
    /// <summary>Stable per-notification identity (a hex GUID) so the InfoBar
    /// list can track and remove this exact entry.</summary>
    public string Id { get; } = Guid.NewGuid().ToString("N");

    /// <summary>x:Bind target for the InfoBar action button (no-op when the
    /// notification carries no action).</summary>
    public void Invoke() => Action?.Invoke();
}
