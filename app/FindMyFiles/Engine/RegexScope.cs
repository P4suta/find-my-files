namespace FindMyFiles.Engine;

/// <summary>Which haystack a whole-query regex runs against (wire values of
/// fmf-core's <c>RegexScope</c>; the <c>regex_mode</c> bit1).</summary>
public enum RegexScope
{
    /// <summary>Match the file name.</summary>
    Name = 0,

    /// <summary>Match the full path.</summary>
    Path = 1,
}
