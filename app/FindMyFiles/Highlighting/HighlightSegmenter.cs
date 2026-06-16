namespace FindMyFiles.Highlighting;

/// <summary>
/// Splits a string into consecutive highlighted/plain segments from a set of
/// merged ranges — the pure, unit-tested core of the highlight renderer, kept
/// free of WinUI so it runs headless. Ranges are clamped to the string, so a
/// stale or out-of-bounds range can never throw.
/// </summary>
public static class HighlightSegmenter
{
    /// <summary>
    /// Cut <paramref name="text"/> at the boundaries of <paramref name="ranges"/>
    /// (assumed sorted and non-overlapping — as produced by
    /// <see cref="CompiledHighlighter.Ranges"/>), tagging each piece as
    /// highlighted or not. Empty ranges yield the whole string as one plain
    /// segment; zero-length or out-of-range entries are skipped/clamped.
    /// </summary>
    /// <param name="text">The full string being rendered.</param>
    /// <param name="ranges">The spans to emphasize.</param>
    /// <returns>The string split into consecutive plain and highlighted pieces, in order.</returns>
    public static List<HighlightSegment> Split(string text, IReadOnlyList<HighlightRange> ranges)
    {
        var segments = new List<HighlightSegment>();
        var pos = 0;
        foreach (var r in ranges)
        {
            var start = Math.Clamp(r.Start, 0, text.Length);
            var end = Math.Clamp(r.Start + r.Length, start, text.Length);
            if (start > pos)
            {
                segments.Add(new HighlightSegment(text[pos..start], false));
            }

            if (end > start)
            {
                segments.Add(new HighlightSegment(text[start..end], true));
            }

            pos = end;
        }

        if (pos < text.Length)
        {
            segments.Add(new HighlightSegment(text[pos..], false));
        }

        return segments;
    }
}
