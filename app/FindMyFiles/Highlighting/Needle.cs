namespace FindMyFiles.Highlighting;

/// <summary>One compiled highlight term: the literal to find (already folded
/// when <see cref="Insensitive"/>), how it is anchored, and which displayed
/// string it targets.</summary>
internal sealed record Needle(string Pattern, bool Insensitive, HighlightField Field, NeedleKind Kind);
