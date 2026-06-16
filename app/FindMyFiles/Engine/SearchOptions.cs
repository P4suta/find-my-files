namespace FindMyFiles.Engine;

/// <summary>The knobs that shape a search, passed to
/// <see cref="IEngineClient.SearchAsync"/> (the C# face of fmf-core's
/// <c>FmfQueryOptions</c>).</summary>
/// <param name="Sort">Which key to order results by.</param>
/// <param name="Descending">True for descending order; false for
/// ascending.</param>
/// <param name="Case">How query case is matched against names.</param>
/// <param name="IncludeHiddenSystem">When true, hidden/system entries are
/// included; they are excluded by default.</param>
/// <param name="RegexMode">When true, the whole query text is one regular
/// expression (the <c>regex:</c> per-term syntax still works regardless).</param>
/// <param name="Scope">Which haystack the whole-query regex matches against
/// (ignored unless <paramref name="RegexMode"/>).</param>
public sealed record SearchOptions(
    FmfSort Sort,
    bool Descending,
    FmfCase Case,
    bool IncludeHiddenSystem = false,
    bool RegexMode = false,
    RegexScope Scope = RegexScope.Name)
{
    /// <summary>The app's default search: by <see cref="FmfSort.Name"/>,
    /// ascending, <see cref="FmfCase.Smart"/> case, hidden/system
    /// excluded, regex mode off.</summary>
    public static readonly SearchOptions Default = new(FmfSort.Name, false, FmfCase.Smart);

    /// <summary>The packed <c>FmfQueryOptions.regex_mode</c> wire value:
    /// bit0 = whole-query regex on, bit1 = scope (0 name / 1 path).</summary>
    public uint RegexModeBits =>
        (RegexMode ? 1u : 0u) | (Scope == RegexScope.Path ? 2u : 0u);
}
