using System.Text;

namespace FindMyFiles.Highlighting;

/// <summary>
/// A query compiled into the positive text needles worth highlighting. Pure
/// (no WinUI), so it is unit-testable and reusable across rows. Built once per
/// query by <see cref="MatchHighlighter.Compile"/> and queried per displayed
/// string by <see cref="Ranges"/>.
///
/// Highlighting deliberately mirrors only the *simple, certain* slice of the
/// engine's match semantics (fmf-core query/): plain substrings, anchored
/// <c>lit*</c>/<c>*lit</c>/<c>*lit*</c> wildcards, and smart-case folding
/// (engine/crates/fmf-core/src/wtf8.rs). Anything the UI cannot reproduce
/// exactly — user regex, general wildcards, negation, path wildcards — is left
/// un-highlighted rather than guessed at: a row the engine returned is never
/// hidden, only its emphasis is skipped (don't crash / don't go silent). This guarantees
/// the UI never lights up a position the engine did not actually match.
/// </summary>
public sealed class CompiledHighlighter : IHighlighter
{
    /// <summary>A highlighter with no terms — every <see cref="Ranges"/> call
    /// returns empty. Used for empty/filter-only queries.</summary>
    public static readonly CompiledHighlighter Empty = new([]);

    private readonly IReadOnlyList<Needle> _needles;

    internal CompiledHighlighter(IReadOnlyList<Needle> needles) => _needles = needles;

    /// <summary>True when nothing will ever be highlighted — the caller can
    /// skip per-row work entirely.</summary>
    public bool IsEmpty => _needles.Count == 0;

    /// <summary>
    /// The ranges of <paramref name="text"/> matched by the terms targeting
    /// <paramref name="field"/>, sorted by start and with overlapping/adjacent
    /// runs merged. Indices/lengths are UTF-16 units of <paramref name="text"/>
    /// itself, ready to slice. Empty when nothing matches (or the query has no
    /// terms for this field).
    /// </summary>
    /// <param name="text">The displayed string (a name, or a full path).</param>
    /// <param name="field">Which term family to apply.</param>
    /// <returns>The sorted, merged highlight ranges; empty when nothing matches.</returns>
    public IReadOnlyList<HighlightRange> Ranges(string text, HighlightField field)
    {
        if (_needles.Count == 0 || text.Length == 0)
        {
            return [];
        }

        // The folded haystack is shared by all insensitive needles for this
        // text; computed lazily because a sensitive-only query never needs it.
        string? folded = null;
        var foldComputed = false;
        var ranges = new List<HighlightRange>();
        foreach (var n in _needles)
        {
            if (n.Field != field)
            {
                continue;
            }

            string hay;
            if (n.Insensitive)
            {
                if (!foldComputed)
                {
                    folded = FoldString(text);
                    foldComputed = true;
                }

                if (folded is null)
                {
                    continue; // text not length-preservingly foldable — skip safely
                }

                hay = folded;
            }
            else
            {
                hay = text;
            }

            CollectMatches(hay, n, ranges);
        }

        return MergeRanges(ranges);
    }

    private static void CollectMatches(string hay, Needle n, List<HighlightRange> ranges)
    {
        switch (n.Kind)
        {
            case NeedleKind.Substring:
                var from = 0;
                while (from <= hay.Length - n.Pattern.Length)
                {
                    var idx = hay.IndexOf(n.Pattern, from, StringComparison.Ordinal);
                    if (idx < 0)
                    {
                        break;
                    }

                    ranges.Add(new HighlightRange(idx, n.Pattern.Length));
                    from = idx + n.Pattern.Length; // non-overlapping occurrences
                }

                break;

            case NeedleKind.Prefix:
                if (hay.StartsWith(n.Pattern, StringComparison.Ordinal))
                {
                    ranges.Add(new HighlightRange(0, n.Pattern.Length));
                }

                break;

            case NeedleKind.Suffix:
                if (hay.EndsWith(n.Pattern, StringComparison.Ordinal))
                {
                    ranges.Add(new HighlightRange(hay.Length - n.Pattern.Length, n.Pattern.Length));
                }

                break;

            default:
                break;
        }
    }

    /// <summary>Sort by start and fuse overlapping or touching runs so the
    /// renderer emits the fewest possible <c>Run</c>s. Shared with
    /// <c>ResultRow</c>, which re-merges name ranges after splitting a path
    /// match at the parent/name boundary.</summary>
    /// <param name="ranges">The unsorted ranges to sort and fuse in place.</param>
    /// <returns>The sorted ranges with overlapping or touching runs merged.</returns>
    internal static List<HighlightRange> MergeRanges(List<HighlightRange> ranges)
    {
        if (ranges.Count <= 1)
        {
            return ranges;
        }

        ranges.Sort(static (a, b) => a.Start.CompareTo(b.Start));
        var merged = new List<HighlightRange>(ranges.Count);
        var cur = ranges[0];
        for (var i = 1; i < ranges.Count; i++)
        {
            var r = ranges[i];
            var curEnd = cur.Start + cur.Length;
            if (r.Start <= curEnd)
            {
                var newEnd = Math.Max(curEnd, r.Start + r.Length);
                cur = new HighlightRange(cur.Start, newEnd - cur.Start);
            }
            else
            {
                merged.Add(cur);
                cur = r;
            }
        }

        merged.Add(cur);
        return merged;
    }

    // ── Folding (engine/crates/fmf-core/src/wtf8.rs, kept byte-for-byte aligned) ──

    /// <summary>
    /// Fold a string with the engine's rule (wtf8::fold_str): lowercase each
    /// scalar only when the result is a single scalar of identical encoded
    /// length, else keep it. The fold preserves UTF-16 length, so an index
    /// found in the folded form is valid in the original. Returns <c>null</c>
    /// if (defensively) some scalar broke length preservation, so the caller
    /// can skip rather than emit a misaligned range.
    /// </summary>
    /// <param name="s">The string to fold.</param>
    /// <returns>The length-preserving folded string, or <c>null</c> if a scalar broke length preservation.</returns>
    internal static string? FoldString(string s)
    {
        var sb = new StringBuilder(s.Length);
        foreach (var rune in s.EnumerateRunes())
        {
            var folded = FoldRune(rune);
            if (folded.Utf16SequenceLength != rune.Utf16SequenceLength)
            {
                return null;
            }

            sb.Append(folded.ToString());
        }

        return sb.Length == s.Length ? sb.ToString() : null;
    }

    /// <summary>True if folding changes <paramref name="s"/> — i.e. it carries
    /// a foldable (uppercase-ish) scalar. Mirrors wtf8::has_uppercase and
    /// drives smart-case: a needle with one is matched case-sensitively.</summary>
    /// <param name="s">The string to inspect.</param>
    /// <returns>True when folding would change the string; otherwise false.</returns>
    internal static bool HasFoldUppercase(string s)
    {
        foreach (var rune in s.EnumerateRunes())
        {
            if (FoldRune(rune) != rune)
            {
                return true;
            }
        }

        return false;
    }

    /// <summary>Lowercase one scalar only if the result is a single scalar of
    /// the same UTF-8 length; otherwise return it unchanged (wtf8::fold_char).
    /// Uses full (ICU) lowercasing via the string overload so multi-scalar
    /// expansions like <c>İ</c>→<c>i̇</c> are detected and kept, matching the
    /// engine (a simple per-rune lowercase would diverge there).</summary>
    [System.Diagnostics.CodeAnalysis.SuppressMessage(
        "Globalization",
        "CA1308:Normalize strings to uppercase",
        Justification = "Mirrors the engine's lowercase case-fold (wtf8.rs fold_char); the index pool is lowercase-folded, so uppercasing would misalign highlight ranges.")]
    private static Rune FoldRune(Rune r)
    {
        if (r.IsAscii)
        {
            return new Rune(char.ToLowerInvariant((char)r.Value));
        }

        var lowered = r.ToString().ToLowerInvariant();
        var it = lowered.EnumerateRunes();
        if (!it.MoveNext())
        {
            return r;
        }

        var first = it.Current;
        if (it.MoveNext())
        {
            return r; // expanded to multiple scalars → keep original
        }

        return Utf8Len(first.Value) == Utf8Len(r.Value) ? first : r;
    }

    private static int Utf8Len(int cp) =>
        cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x1_0000 ? 3 : 4;
}
