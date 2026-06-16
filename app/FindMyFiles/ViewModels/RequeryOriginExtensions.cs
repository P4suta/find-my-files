namespace FindMyFiles.ViewModels;

/// <summary>Helpers over <see cref="RequeryOrigin"/>.</summary>
public static class RequeryOriginExtensions
{
    /// <summary>
    /// True for origins that restore the previous viewport instead of scrolling
    /// to the top (<see cref="RequeryOrigin.IndexChanged"/>,
    /// <see cref="RequeryOrigin.VolumeReady"/>, <see cref="RequeryOrigin.Stale"/>) —
    /// the background refreshes the user did not initiate.
    /// </summary>
    /// <param name="origin">The requery origin to classify.</param>
    /// <returns>True when the origin preserves the viewport; false when it resets to the top.</returns>
    public static bool PreservesPosition(this RequeryOrigin origin) =>
        origin is RequeryOrigin.IndexChanged or RequeryOrigin.VolumeReady or RequeryOrigin.Stale;
}
