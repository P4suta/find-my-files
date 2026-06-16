using System.Text;
using System.Text.RegularExpressions;
using FindMyFiles.Engine;

namespace FindMyFiles.Highlighting;

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
    /// <returns>A highlighter for the query, or <see cref="CompiledHighlighter.Empty"/> when nothing is highlightable.</returns>
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
    /// <param name="query">Raw user query text, treated as one regex pattern.</param>
    /// <param name="scope">Which haystack the pattern runs against (name or full path).</param>
    /// <returns>A regex highlighter, or <see cref="CompiledHighlighter.Empty"/> when the pattern cannot compile.</returns>
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
