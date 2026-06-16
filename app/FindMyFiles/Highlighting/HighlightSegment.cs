namespace FindMyFiles.Highlighting;

/// <summary>One piece of a string for highlighted rendering.</summary>
/// <param name="Text">The piece's text.</param>
/// <param name="Highlighted">True when this piece should be emphasized.</param>
public readonly record struct HighlightSegment(string Text, bool Highlighted);
