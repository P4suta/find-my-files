using System.Text.RegularExpressions;
using FindMyFiles.Engine;

namespace FindMyFiles.Highlighting;

/// <summary>
/// Highlights a whole-query regex (regex mode) by re-matching the displayed
/// string. Emphasizes the name field for a name-scope pattern and the path
/// field for a path-scope one — the same field the engine matched, so
/// <c>ResultRow</c>'s parent/name split applies unchanged. A match-timeout (or
/// any mismatch with the engine's rust regex) simply yields no ranges: a row
/// the engine returned is never hidden, only its emphasis is skipped.
/// </summary>
public sealed class RegexHighlighter(Regex re, RegexScope scope) : IHighlighter
{
    /// <inheritdoc/>
    public bool IsEmpty => false;

    /// <inheritdoc/>
    public IReadOnlyList<HighlightRange> Ranges(string text, HighlightField field)
    {
        // The pattern matched one haystack: the name (name scope) or the full
        // path (path scope). Only light that field.
        var target = scope == RegexScope.Path ? HighlightField.Path : HighlightField.Name;
        if (field != target || text.Length == 0)
        {
            return [];
        }

        try
        {
            var ranges = new List<HighlightRange>();
            foreach (Match m in re.Matches(text))
            {
                if (m.Length > 0)
                {
                    ranges.Add(new HighlightRange(m.Index, m.Length));
                }
            }

            return CompiledHighlighter.MergeRanges(ranges);
        }
        catch (RegexMatchTimeoutException)
        {
            return []; // skip safely rather than light up a guess
        }
    }
}
