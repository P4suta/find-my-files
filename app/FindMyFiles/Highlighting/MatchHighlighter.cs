using System.Text;
using System.Text.RegularExpressions;
using FindMyFiles.Engine;

namespace FindMyFiles.Highlighting;

/// <summary>
/// Which displayed string a highlight term is matched against — the file
/// name leaf, or the full path (parent + name).
/// </summary>
public enum HighlightField
{
    /// <summary>The file/folder name (leaf).</summary>
    Name,

    /// <summary>The full path — a term that contained <c>\</c> or used
    /// <c>path:</c>.</summary>
    Path,
}

/// <summary>
/// A half-open run of UTF-16 code units to emphasize, in the coordinate space
/// of the string handed to <see cref="CompiledHighlighter.Ranges"/>.
/// </summary>
/// <param name="Start">Zero-based UTF-16 index of the first highlighted unit.</param>
/// <param name="Length">Number of UTF-16 units highlighted (always ≥ 1).</param>
public readonly record struct HighlightRange(int Start, int Length);

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
    IReadOnlyList<HighlightRange> Ranges(string text, HighlightField field);
}

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
/// hidden, only its emphasis is skipped (落ちない・黙らない). This guarantees
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

/// <summary>How a needle is anchored against the haystack.</summary>
internal enum NeedleKind
{
    /// <summary>Match anywhere (plain term, or <c>*lit*</c>).</summary>
    Substring,

    /// <summary>Match only at the start (<c>lit*</c>).</summary>
    Prefix,

    /// <summary>Match only at the end (<c>*lit</c>).</summary>
    Suffix,
}

/// <summary>One compiled highlight term: the literal to find (already folded
/// when <see cref="Insensitive"/>), how it is anchored, and which displayed
/// string it targets.</summary>
internal sealed record Needle(string Pattern, bool Insensitive, HighlightField Field, NeedleKind Kind);

/// <summary>
/// Compiles query text into a <see cref="CompiledHighlighter"/>. Mirrors the
/// engine tokenizer/compiler (engine/crates/fmf-core/src/query/ast.rs and
/// compile.rs) for the subset that can be highlighted exactly, and drops the
/// rest (see <see cref="CompiledHighlighter"/>).
/// </summary>
public static class MatchHighlighter
{
    /// <summary>Highlight any non-empty literal, down to a single character.
    /// Modern search UIs (editors, browser find) light every match from the
    /// first keystroke; the engine substring-matches 1-char queries too, so
    /// skipping them in the highlight only desynced the two. A lone "a"
    /// lighting up is the natural cost of a 1-char query and resolves the
    /// moment a second character is typed. (Empty needles never reach here —
    /// blank atoms are dropped during tokenization.)</summary>
    private const int MinHighlightLength = 1;

    private static readonly string[] Fields =
        ["ext", "path", "size", "dm", "regex", "file", "folder"];

    /// <summary>
    /// Compile <paramref name="query"/> (the user's raw text, before any
    /// focused-mode rewrite) into a highlighter. Case handling is smart-case,
    /// matching the product default (MainViewModel uses
    /// <c>FmfCase.Smart</c>). An empty/whitespace or
    /// filter-only query yields <see cref="CompiledHighlighter.Empty"/>.
    /// </summary>
    /// <param name="query">Raw user query text.</param>
    public static CompiledHighlighter Compile(string query)
    {
        if (string.IsNullOrWhiteSpace(query))
        {
            return CompiledHighlighter.Empty;
        }
        var needles = new List<Needle>();
        foreach (var (atom, negated) in Tokenize(query))
        {
            if (negated)
            {
                continue; // a negated term has nothing to emphasize
            }
            AddTerm(atom, needles);
        }
        return needles.Count == 0 ? CompiledHighlighter.Empty : new CompiledHighlighter(needles);
    }

    /// <summary>
    /// Compile a whole-query regex highlighter (regex mode, ADR-0023): the
    /// entire <paramref name="query"/> is one pattern, emphasized in the name
    /// or full path per <paramref name="scope"/>. Smart-case (insensitive
    /// unless the pattern carries an uppercase-ish scalar), matching the
    /// product default. A pattern .NET cannot compile yields
    /// <see cref="CompiledHighlighter.Empty"/> — the engine reports the syntax
    /// error; the highlighter just stays dark rather than guess.
    /// </summary>
    public static IHighlighter CompileRegex(string query, RegexScope scope)
    {
        if (string.IsNullOrEmpty(query))
        {
            return CompiledHighlighter.Empty;
        }
        var options = RegexOptions.Singleline
            | (CompiledHighlighter.HasFoldUppercase(query)
                ? RegexOptions.None
                : RegexOptions.IgnoreCase);
        try
        {
            // .NET regex differs from the engine's rust regex, so a re-match
            // can disagree; "skip safely" (no highlight) is preferred to
            // lighting up a guessed position. The 100ms timeout caps a
            // pathological pattern.
            var re = new Regex(query, options, TimeSpan.FromMilliseconds(100));
            return new RegexHighlighter(re, scope);
        }
        catch (ArgumentException)
        {
            return CompiledHighlighter.Empty;
        }
    }

    /// <summary>Walk the query into (atom, negated) pairs, honoring quotes and
    /// treating <c>|</c> as a separator — OR groups are unioned, since the UI
    /// cannot know which group a row matched (mirrors ast.rs tokenization, but
    /// tolerant of an unclosed quote instead of erroring).</summary>
    private static IEnumerable<(string Atom, bool Negated)> Tokenize(string input)
    {
        var i = 0;
        while (i < input.Length)
        {
            while (i < input.Length && char.IsWhiteSpace(input[i]))
            {
                i++;
            }
            if (i >= input.Length)
            {
                break;
            }
            if (input[i] == '|')
            {
                i++; // group separator — unioned
                continue;
            }
            var negated = false;
            while (i < input.Length && input[i] == '!')
            {
                negated = !negated;
                i++;
            }
            var sb = new StringBuilder();
            var inQuotes = false;
            while (i < input.Length)
            {
                var c = input[i];
                if (c == '"')
                {
                    inQuotes = !inQuotes;
                    sb.Append(c);
                    i++;
                }
                else if (!inQuotes && (char.IsWhiteSpace(c) || c == '|'))
                {
                    break;
                }
                else
                {
                    sb.Append(c);
                    i++;
                }
            }
            if (sb.Length > 0)
            {
                yield return (sb.ToString(), negated);
            }
        }
    }

    /// <summary>Turn one positive atom into needle(s): recognize the
    /// highlightable <c>field:value</c> forms, else treat it as a name/path
    /// term (ast.rs terms_from_atom + name_or_path_term).</summary>
    private static void AddTerm(string atom, List<Needle> needles)
    {
        if (!atom.StartsWith('"'))
        {
            var colon = atom.IndexOf(':', StringComparison.Ordinal);
            if (colon >= 0)
            {
                var field = atom[..colon];
                var fieldIndex = Array.FindIndex(
                    Fields, f => f.Equals(field, StringComparison.OrdinalIgnoreCase));
                if (fieldIndex >= 0)
                {
                    AddFieldTerm(Fields[fieldIndex], atom[(colon + 1)..], needles);
                    return;
                }
            }
        }
        AddNameOrPath(Unquote(atom), needles);
    }

    private static void AddFieldTerm(string field, string rawValue, List<Needle> needles)
    {
        switch (field)
        {
            case "path":
                var p = Unquote(rawValue);
                if (!HasWildcard(p))
                {
                    AddNeedle(p, HighlightField.Path, NeedleKind.Substring, needles);
                }
                break; // path wildcard → regex over path, skipped

            case "file":
            case "folder":
                // `file:`/`folder:` expand to [IsDir, name_or_path_term(value)]
                // — the type flag is invisible, the value part is highlightable.
                var v = Unquote(rawValue);
                if (v.Length > 0)
                {
                    AddNameOrPath(v, needles);
                }
                break;

            default:
                // ext: / size: / dm: are filters; regex: differs from .NET regex.
                break;
        }
    }

    /// <summary>name_or_path_term: <c>\</c> picks path, <c>*</c>/<c>?</c> picks
    /// wildcard; the plain case is a name substring.</summary>
    private static void AddNameOrPath(string value, List<Needle> needles)
    {
        var wild = HasWildcard(value);
        var pathy = value.Contains('\\', StringComparison.Ordinal);
        if (pathy)
        {
            if (!wild)
            {
                AddNeedle(value, HighlightField.Path, NeedleKind.Substring, needles);
            }
            return; // path wildcard → regex over path, skipped
        }
        if (wild)
        {
            AddWildcard(value, needles);
            return;
        }
        AddNeedle(value, HighlightField.Name, NeedleKind.Substring, needles);
    }

    /// <summary>classify_wildcard: only the anchored <c>lit*</c>/<c>*lit</c>/
    /// <c>*lit*</c> shapes are highlightable (their literal is exact); a
    /// <c>?</c>, an inner <c>*</c>, or a bare <c>*</c> become a general regex
    /// and are skipped.</summary>
    private static void AddWildcard(string pattern, List<Needle> needles)
    {
        if (pattern.Contains('?', StringComparison.Ordinal))
        {
            return;
        }
        var starts = pattern.StartsWith('*');
        var ends = pattern.EndsWith('*');
        var inner = pattern.Trim('*');
        if (inner.Length == 0 || inner.Contains('*', StringComparison.Ordinal))
        {
            return; // "*", "**", "a*b"
        }
        if (starts && ends)
        {
            AddNeedle(inner, HighlightField.Name, NeedleKind.Substring, needles);
        }
        else if (starts)
        {
            AddNeedle(inner, HighlightField.Name, NeedleKind.Suffix, needles); // *lit
        }
        else if (ends)
        {
            AddNeedle(inner, HighlightField.Name, NeedleKind.Prefix, needles); // lit*
        }
    }

    /// <summary>Add a literal needle: enforce the min length, decide smart-case
    /// (a foldable char ⇒ sensitive), and pre-fold for the insensitive case so
    /// matching is a plain ordinal compare against the folded haystack.</summary>
    private static void AddNeedle(string literal, HighlightField field, NeedleKind kind, List<Needle> needles)
    {
        if (literal.Length < MinHighlightLength)
        {
            return;
        }
        var insensitive = !CompiledHighlighter.HasFoldUppercase(literal);
        string pattern;
        if (insensitive)
        {
            var folded = CompiledHighlighter.FoldString(literal);
            if (folded is null)
            {
                return; // not length-preservingly foldable — skip safely
            }
            pattern = folded;
        }
        else
        {
            pattern = literal;
        }
        needles.Add(new Needle(pattern, insensitive, field, kind));
    }

    private static string Unquote(string s) =>
        s.Length >= 2 && s[0] == '"' && s[^1] == '"' ? s[1..^1] : s;

    private static bool HasWildcard(string s) =>
        s.Contains('*', StringComparison.Ordinal) || s.Contains('?', StringComparison.Ordinal);
}

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
