namespace FindMyFiles.Services;

/// <summary>Pure path helpers for scope mode (ADR-0024). The engine treats
/// scope roots as independent and would double-walk a nested pair, so the UI
/// normalizes the chosen set before persisting. No I/O — unit-tested directly.</summary>
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

    /// <summary>True when <paramref name="parent"/> equals or contains
    /// <paramref name="path"/>. Separator-aware so <c>C:\A</c> does not cover
    /// <c>C:\AB</c>, and case-insensitive (NTFS). Compared on separator-trimmed
    /// copies so a trailing <c>\</c> never changes the verdict.</summary>
    private static bool Covers(string parent, string path)
    {
        var p = parent.TrimEnd('\\');
        var c = path.TrimEnd('\\');
        if (string.Equals(p, c, StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }

        return c.Length > p.Length
            && c.StartsWith(p, StringComparison.OrdinalIgnoreCase)
            && c[p.Length] == '\\';
    }
}
