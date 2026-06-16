namespace FindMyFiles.Highlighting;

/// <summary>
/// A compiled query that can emphasize the slices of a displayed string it
/// matched. Implemented by the substring/wildcard <see cref="CompiledHighlighter"/>
/// and the whole-query <see cref="RegexHighlighter"/>; the results list and rows
/// depend only on this seam.
/// </summary>
public interface IHighlighter
{
    /// <summary>True when nothing will ever be highlighted — the caller can
    /// skip per-row work entirely.</summary>
    bool IsEmpty { get; }

    /// <summary>The ranges of <paramref name="text"/> to emphasize for
    /// <paramref name="field"/> (UTF-16 units of <paramref name="text"/>),
    /// sorted and merged; empty when nothing matches.</summary>
    /// <param name="text">The displayed string (a name, or a full path).</param>
    /// <param name="field">Which displayed string the ranges target.</param>
    /// <returns>The sorted, merged highlight ranges; empty when nothing matches.</returns>
    IReadOnlyList<HighlightRange> Ranges(string text, HighlightField field);
}
