using System.Text;
using FindMyFiles.Services;

namespace FindMyFiles.ViewModels;

/// <summary>
/// 絞り込みモード (focused search): a pure query rewrite in the UI layer —
/// the engine is never touched (ADR-0019). Every top-level OR group of the
/// user's query gets the configured noise-path exclusions (<c>!path:"…"</c>)
/// and the extension whitelist (one <c>ext:a;b;…</c> term) appended, except
/// where the user already expressed intent: a group mentioning
/// <c>ext:</c>/<c>regex:</c> keeps its own type filter, a group mentioning
/// <c>path:</c> or containing <c>\</c> keeps its own location.
/// </summary>
public static class FocusedQueryRewriter
{
    /// <summary>Values already warned about — a bad settings entry must be
    /// said once (黙らない), not on every keystroke (2MB log rotation).</summary>
    private static readonly HashSet<string> WarnedValues = [];

    /// <summary>Rewrites <paramref name="userQuery"/> for focused mode.
    /// An empty/whitespace query is returned unchanged — the "no query, no
    /// results" rule stays the orchestrator's, and a rewrite must never turn
    /// an empty query into a non-empty one.</summary>
    /// <param name="userQuery">The user's raw query text.</param>
    /// <param name="excludePaths">Noise paths appended as <c>!path:"…"</c> exclusions.</param>
    /// <param name="extensions">The extension whitelist appended as one <c>ext:</c> term.</param>
    /// <returns>The rewritten query, or the input unchanged when empty or already constrained.</returns>
    public static string Compose(
        string userQuery,
        IReadOnlyList<string> excludePaths,
        IReadOnlyList<string> extensions)
    {
        if (string.IsNullOrWhiteSpace(userQuery))
        {
            return userQuery;
        }

        var excludeSuffix = BuildExcludeSuffix(excludePaths);
        var extSuffix = BuildExtSuffix(extensions);
        if (excludeSuffix.Length == 0 && extSuffix.Length == 0)
        {
            return userQuery;
        }

        var groups = SplitTopLevelOr(userQuery);
        for (var i = 0; i < groups.Count; i++)
        {
            var group = groups[i];

            // Simple substring heuristics on the user's group (quoted
            // occurrences over-match, which only skips a suffix — the user's
            // terms always win). Both flags are decided before any append:
            // the exclude suffix itself contains path:.
            var hasLocation = ContainsIgnoreCase(group, "path:") || group.Contains('\\', StringComparison.Ordinal);
            var hasTypeFilter =
                ContainsIgnoreCase(group, "ext:") || ContainsIgnoreCase(group, "regex:");
            if (!hasLocation)
            {
                group += excludeSuffix;
            }

            if (!hasTypeFilter)
            {
                group += extSuffix;
            }

            groups[i] = group;
        }

        return string.Join(" | ", groups);
    }

    /// <summary>Top-level split on <c>|</c>, leaving <c>|</c> inside quoted
    /// sections alone (mirrors the engine tokenizer's quote handling).</summary>
    private static List<string> SplitTopLevelOr(string query)
    {
        var groups = new List<string>();
        var current = new StringBuilder();
        var inQuotes = false;
        foreach (var c in query)
        {
            if (c == '"')
            {
                inQuotes = !inQuotes;
            }

            if (c == '|' && !inQuotes)
            {
                groups.Add(current.ToString().Trim());
                current.Clear();
            }
            else
            {
                current.Append(c);
            }
        }

        groups.Add(current.ToString().Trim());
        return groups;
    }

    private static string BuildExcludeSuffix(IReadOnlyList<string> excludePaths)
    {
        var sb = new StringBuilder();
        foreach (var raw in excludePaths)
        {
            var p = raw?.Trim();
            if (string.IsNullOrEmpty(p))
            {
                continue;
            }

            // A quote inside the value cannot be escaped in the query
            // language — it would silently change the whole query's meaning.
            if (p.Contains('"', StringComparison.Ordinal))
            {
                WarnOnce("focused", $"exclude path with a quote ignored: {p}");
                continue;
            }

            sb.Append(" !path:\"").Append(p).Append('"');
        }

        return sb.ToString();
    }

    private static string BuildExtSuffix(IReadOnlyList<string> extensions)
    {
        var valid = new List<string>();
        foreach (var raw in extensions)
        {
            var e = raw?.Trim();
            if (string.IsNullOrEmpty(e))
            {
                continue;
            }

            // ext: is an unquoted atom — whitespace/quote/pipe would break
            // tokenization; ;/, are the engine's own list separators.
            if (e.Any(char.IsWhiteSpace) || e.AsSpan().ContainsAny("\"|;,"))
            {
                WarnOnce("focused", $"extension entry ignored: {e}");
                continue;
            }

            valid.Add(e);
        }

        // No valid entries → no term at all (an empty ext: matches nothing).
        return valid.Count == 0 ? string.Empty : " ext:" + string.Join(';', valid);
    }

    private static void WarnOnce(string area, string message)
    {
        lock (WarnedValues)
        {
            if (!WarnedValues.Add(message))
            {
                return;
            }
        }

        FileLog.Warn(area, message + " (settings.json)");
    }

    private static bool ContainsIgnoreCase(string haystack, string needle) =>
        haystack.Contains(needle, StringComparison.OrdinalIgnoreCase);
}
