namespace FindMyFiles.Engine;

/// <summary>The query string is syntactically malformed and
/// <see cref="IEngineClient.SearchAsync(string, SearchOptions, CancellationToken)"/>
/// failed to parse it.</summary>
/// <param name="message">The human-readable reason returned by the parser.</param>
internal sealed class QuerySyntaxException(string message) : Exception(message);
