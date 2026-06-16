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
