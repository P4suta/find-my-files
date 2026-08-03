namespace FindMyFiles.Services;

/// <summary>Owned service handle boundary used by setup policy.</summary>
internal interface IServiceHandle : IDisposable
{
    /// <summary>Reads the current SCM lifecycle state.</summary>
    /// <param name="state">Raw Win32 service state on success.</param>
    /// <returns>True when the state was read.</returns>
    bool TryQueryState(out uint state);

    /// <summary>Queries the buffer size needed for the service description.</summary>
    /// <returns>The required buffer size in bytes, or zero on failure.</returns>
    uint QueryDescriptionBytesNeeded();

    /// <summary>Reads the service description into managed memory.</summary>
    /// <param name="bytesNeeded">Previously queried buffer size.</param>
    /// <param name="description">Service description on success.</param>
    /// <returns>True when the description was read.</returns>
    bool TryReadDescription(uint bytesNeeded, out string? description);

    /// <summary>Reads the service state and process id.</summary>
    /// <param name="state">Raw Win32 service state on success.</param>
    /// <param name="processId">Service process id on success.</param>
    /// <returns>True when extended status was read.</returns>
    bool TryQueryProcess(out uint state, out uint processId);

    /// <summary>Issues a service start request.</summary>
    /// <returns>Zero on success, otherwise the Win32 error.</returns>
    int Start();

    /// <summary>Issues a service stop request.</summary>
    /// <returns>Zero on success, otherwise the Win32 error.</returns>
    int Stop();
}
