namespace FindMyFiles.Engine;

/// <summary>Case-matching mode (wire values of fmf-core's <c>CaseMode</c>).</summary>
public enum FmfCase
{
    /// <summary>Case-insensitive unless the query contains an uppercase
    /// letter, in which case it becomes case-sensitive.</summary>
    Smart = 0,

    /// <summary>Always case-insensitive.</summary>
    Insensitive = 1,

    /// <summary>Always case-sensitive.</summary>
    Sensitive = 2,
}
