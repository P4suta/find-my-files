namespace FindMyFiles.Services;

/// <summary>Pure close-vs-hide decision for the main window (ADR-0030), extracted
/// so the tray-resident close branch is unit-testable without a live window.</summary>
internal static class WindowLifecycle
{
    /// <summary>Whether a close (×) request should hide the window to the tray
    /// instead of letting it close/exit.</summary>
    /// <param name="closeToTray">The persisted tray-resident setting.</param>
    /// <param name="explicitExit">True when the user chose the tray menu's "Exit"
    /// (or another real-quit path) — always allow the close.</param>
    /// <returns>True to hide to tray (cancel the close); false to let it close.</returns>
    internal static bool ShouldHideToTray(bool closeToTray, bool explicitExit) =>
        closeToTray && !explicitExit;
}
