namespace FindMyFiles.Services;

/// <summary>Trusted helper path plus the lease that pins its verified image.</summary>
/// <param name="path">Verified full helper path.</param>
/// <param name="lease">Image-lock lease retained through process completion.</param>
internal sealed class TrustedServiceExecutable(string path, IDisposable lease) : IDisposable
{
    /// <summary>Gets the verified helper path.</summary>
    internal string Path { get; } = path;

    /// <inheritdoc/>
    public void Dispose() => lease.Dispose();
}
