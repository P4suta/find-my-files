namespace FindMyFiles.Services;

/// <summary>Owned service-control-manager handle boundary.</summary>
internal interface IServiceManagerHandle : IDisposable
{
    /// <summary>Gets the most recent service-open Win32 error.</summary>
    int LastError { get; }

    /// <summary>Opens the named service with the requested access mask.</summary>
    /// <param name="name">SCM service name.</param>
    /// <param name="access">Requested service access mask.</param>
    /// <returns>An owned service handle, or null on failure.</returns>
    IServiceHandle? OpenService(string name, uint access);
}
