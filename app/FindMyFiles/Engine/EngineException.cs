namespace FindMyFiles.Engine;

/// <summary>The engine rejected an operation with a structured error code (the
/// transport is alive but the engine returned a failure). The code roster is
/// <see cref="EngineContract.Status"/> — append-only and shared by the FFI
/// return values and the pipe frame header. Codes that carry their own recovery
/// path never reach here: query syntax becomes <see cref="QuerySyntaxException"/>,
/// a stale result <see cref="StaleResultException"/>, and a cancelled query an
/// <see cref="OperationCanceledException"/>. Everything else is localized by
/// code and surfaced as an InfoBar (MainViewModel.EngineErrorText).</summary>
/// <param name="message">The human-readable message returned by the engine.</param>
/// <param name="code">The numeric `FMF_E_*` code (held in <see cref="Code"/>).</param>
internal sealed class EngineException(string message, int code) : Exception(message)
{
    /// <summary>The `FMF_E_*` code returned by the engine. Used for UI branching
    /// (e.g. `FMF_E_LOCKED` routes to the setup screen).</summary>
    public int Code { get; } = code;
}
