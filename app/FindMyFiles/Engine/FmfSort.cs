namespace FindMyFiles.Engine;

/// <summary>Result sort key (wire values of fmf-core's <c>SortKey</c>).</summary>
public enum FmfSort
{
    /// <summary>Sort by file name.</summary>
    Name = 0,

    /// <summary>Sort by file size in bytes.</summary>
    Size = 1,

    /// <summary>Sort by modification time.</summary>
    Mtime = 2,
}
