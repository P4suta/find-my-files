namespace FindMyFiles.Engine;

/// <summary>The engine rejected an operation with a structured error code (the
/// transport is alive but the engine returned a failure). The code maps to the
/// `FMF_E_*` table in docs/ARCHITECTURE.md.</summary>
/// <param name="message">The human-readable message returned by the engine.</param>
/// <param name="code">The numeric `FMF_E_*` code (held in <see cref="Code"/>).</param>
public sealed class EngineException(string message, int code) : Exception(message)
{
    /// <summary>The `FMF_E_*` code returned by the engine. Used for UI branching
    /// (e.g. `FMF_E_LOCKED` routes to the setup screen).</summary>
    public int Code { get; } = code;
}
