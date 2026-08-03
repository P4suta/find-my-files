namespace FindMyFiles.Services;

/// <summary>Owned elevated helper process boundary.</summary>
internal interface IElevatedProcess : IDisposable
{
    /// <summary>Gets the completed process exit code.</summary>
    int ExitCode { get; }

    /// <summary>Gets the process identifier.</summary>
    int Id { get; }

    /// <summary>Waits for process completion for a bounded interval.</summary>
    /// <param name="milliseconds">Maximum wait in milliseconds.</param>
    /// <returns>True when the process exited.</returns>
    bool WaitForExit(int milliseconds);

    /// <summary>Terminates the process.</summary>
    /// <param name="entireProcessTree">Whether descendants are also terminated.</param>
    void Kill(bool entireProcessTree);
}
