using FindMyFiles.Highlighting;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Pins the highlighter's match semantics — the UI-side subset that must agree
/// with the engine (fmf-core query/ + wtf8.rs). Cases are aligned with the
/// engine's own fixtures (File.TXT, ΣΟΦΟΣ) so a future divergence is caught.
/// </summary>
public sealed class MatchHighlighterTests
{
    private static (int Start, int Length)[] Name(string query, string text) =>
        Ranges(query, text, HighlightField.Name);

    private static (int Start, int Length)[] Path(string query, string text) =>
        Ranges(query, text, HighlightField.Path);

    private static (int Start, int Length)[] Ranges(string query, string text, HighlightField field) =>
        MatchHighlighter.Compile(query).Ranges(text, field)
            .Select(r => (r.Start, r.Length)).ToArray();

    [Fact]
    public void PlainSubstring_Matches() =>
        Assert.Equal([(2, 3)], Name("foo", "myfoofile"));

    [Fact]
    public void SmartCase_LowercaseNeedle_IsInsensitive() =>
        Assert.Equal([(0, 3), (4, 3), (9, 3)], Name("foo", "Foo_FOOZ_foo"));

    [Fact]
    public void SmartCase_UppercaseNeedle_IsSensitive() =>
        Assert.Equal([(4, 3)], Name("Foo", "foo Foo"));

    [Fact]
    public void MultipleTerms_MergeAdjacent() =>
        Assert.Equal([(0, 6)], Name("foo bar", "barfoo"));

    [Fact]
    public void TwoCharNeedle_Highlights() =>
        Assert.Equal([(1, 2)], Name("oo", "foo"));

    [Fact]
    public void SingleCharNeedle_Highlights() => // 1-char queries light up too (no min-length gate)
        Assert.Equal([(1, 1), (3, 1), (5, 1)], Name("a", "banana"));

    [Fact]
    public void WildcardSuffix_LiteralOnly() =>
        Assert.Equal([(4, 3)], Name("*.rs", "main.rs"));

    [Fact]
    public void WildcardPrefix_LiteralOnly() =>
        Assert.Equal([(0, 3)], Name("src*", "src_lib"));

    [Fact]
    public void WildcardInner_Substring() =>
        Assert.Equal([(1, 3)], Name("*foo*", "xfooy"));

    [Fact]
    public void GeneralWildcard_Skipped() =>
        Assert.Empty(Name("a*b", "axb"));

    [Fact]
    public void Regex_Skipped() =>
        Assert.Empty(Name("regex:f.o", "foo"));

    [Fact]
    public void Negation_Skipped() =>
        Assert.Empty(Name("!tmp", "tmpfile"));

    [Fact]
    public void DoubleNegation_Highlights() =>
        Assert.Equal([(0, 3)], Name("!!tmp", "tmpdir"));

    [Fact]
    public void PathTerm_MatchesAgainstPath() =>
        Assert.Equal([(3, 8)], Path(@"docs\rep", @"C:\docs\reports\"));

    [Fact]
    public void PathTerm_NotAppliedToName() =>
        Assert.Empty(Name(@"docs\rep", "report.txt"));

    [Fact]
    public void QuotedPathValue_Matches() =>
        Assert.Equal([(3, 13)], Path(@"path:""Program Files""", @"C:\Program Files\"));

    [Fact]
    public void FolderValue_HighlightsNamePart() =>
        Assert.Equal([(0, 3)], Name("folder:src", "src"));

    [Fact]
    public void FilterOnlyQuery_IsEmpty()
    {
        var hl = MatchHighlighter.Compile("size:>1mb");
        Assert.True(hl.IsEmpty);
        Assert.Empty(hl.Ranges("huge.bin", HighlightField.Name));
    }

    [Fact]
    public void NonAscii_Substring() =>
        Assert.Equal([(3, 2)], Name("日本", "これは日本語"));

    [Fact]
    public void GreekFold_Insensitive() =>
        Assert.Equal([(1, 3)], Name("οφο", "ΣΟΦΟΣ"));

    [Fact]
    public void OrGroups_Unioned() =>
        Assert.Equal([(0, 3)], Name("foo|bar", "bar"));

    [Fact]
    public void UnknownColonAtom_IsName() =>
        Assert.Equal([(0, 5)], Name("12:30", "12:30pm"));

    [Fact]
    public void EmptyQuery_IsEmpty()
    {
        var hl = MatchHighlighter.Compile("   ");
        Assert.True(hl.IsEmpty);
        Assert.Empty(hl.Ranges("anything", HighlightField.Name));
    }

    [Fact]
    public void CaseInsensitiveAscii_MatchesFoldedPosition() =>
        Assert.Equal([(5, 3)], Name("txt", "File.TXT"));

    [Fact]
    public void CaseSensitiveAscii_OnlyExact() =>
        Assert.Equal([(5, 3)], Name("TXT", "File.TXT"));
}
