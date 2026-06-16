namespace FindMyFiles.Highlighting;

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
