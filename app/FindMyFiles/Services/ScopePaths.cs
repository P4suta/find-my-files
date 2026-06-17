namespace FindMyFiles.Services;

/// <summary>Pure path helpers for scope mode (ADR-0024/-0025). The engine treats
/// scope roots as independent and would double-walk a nested pair, so the UI
/// normalizes the chosen set; excludes must sit under a root to mean anything.
/// No I/O — unit-tested directly.</summary>
public static class ScopePaths
{
    /// <summary>Drop any root nested under another selected root (and exact
    /// case-insensitive duplicates), so e.g. <c>{C:\A, C:\A\B}</c> collapses to
    /// <c>{C:\A}</c>. The surviving roots keep their original strings (no
    /// trailing-separator rewriting — <c>C:\</c> must stay <c>C:\</c>) and their
    /// first-seen order.</summary>
    /// <param name="roots">The chosen roots, in selection order.</param>
    /// <returns>The covering roots with nested/duplicate entries removed.</returns>
    public static List<string> Normalize(IEnumerable<string> roots)
    {
        var kept = new List<string>();
        foreach (var root in roots)
        {
            // Already covered by (or equal to) a root we are keeping → skip.
            if (kept.Exists(k => Covers(k, root)))
            {
                continue;
            }

            // This root may now cover ones kept earlier (a parent picked after
            // its child) → drop those, then keep this one.
            kept.RemoveAll(k => Covers(root, k));
            kept.Add(root);
        }

        return kept;
    }

    /// <summary>True when <paramref name="path"/> sits strictly under one of
    /// <paramref name="roots"/> — the guard for adding a scope exclude, which
    /// must be inside the indexed set to prune anything (ADR-0025). Excluding a
    /// whole root (equal path) is rejected: drop the root instead.</summary>
    /// <param name="path">The candidate exclude path.</param>
    /// <param name="roots">The selected scope roots.</param>
    /// <returns>True when the path is a proper descendant of some root.</returns>
    public static bool IsUnderAnyRoot(string path, IEnumerable<string> roots) =>
        roots.Any(r => StrictlyUnder(path, r));

    /// <summary>True when <paramref name="parent"/> equals or contains
    /// <paramref name="path"/> (separator-aware, case-insensitive).</summary>
    private static bool Covers(string parent, string path) =>
        SamePath(parent, path) || StrictlyUnder(path, parent);

    /// <summary>Case-insensitive path equality, ignoring a trailing separator.</summary>
    private static bool SamePath(string a, string b) =>
        string.Equals(a.TrimEnd('\\'), b.TrimEnd('\\'), StringComparison.OrdinalIgnoreCase);

    /// <summary>True when <paramref name="child"/> is a proper descendant of
    /// <paramref name="parent"/> — separator-aware so <c>C:\AB</c> is not under
    /// <c>C:\A</c>, and case-insensitive (NTFS). Compared on separator-trimmed
    /// copies so a trailing <c>\</c> never changes the verdict.</summary>
    private static bool StrictlyUnder(string child, string parent)
    {
        var p = parent.TrimEnd('\\');
        var c = child.TrimEnd('\\');
        return c.Length > p.Length
            && c.StartsWith(p, StringComparison.OrdinalIgnoreCase)
            && c[p.Length] == '\\';
    }
}
