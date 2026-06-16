namespace FindMyFiles.Engine;

/// <summary>The engine transport is down (pipe disconnected, request timed
/// out, service not running). Pending requests fail fast with this; the
/// supervisor keeps reconnecting in the background.</summary>
/// <param name="message">Human-readable description of which transport failure occurred.</param>
public sealed class EngineUnavailableException(string message) : Exception(message);
