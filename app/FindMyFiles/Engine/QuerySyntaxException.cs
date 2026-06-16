namespace FindMyFiles.Engine;

/// <summary>クエリ文字列の構文が不正で <see cref="IEngineClient.SearchAsync"/>
/// がパースに失敗したことを示す。</summary>
/// <param name="message">パーサが返した人間可読な理由。</param>
public sealed class QuerySyntaxException(string message) : Exception(message);
