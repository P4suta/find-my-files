namespace FindMyFiles.Engine;

/// <summary>The query string is syntactically malformed and
/// <see cref="IEngineClient.SearchAsync"/> failed to parse it.</summary>
/// <param name="message">The human-readable reason returned by the parser.</param>
public sealed class QuerySyntaxException(string message) : Exception(message);
