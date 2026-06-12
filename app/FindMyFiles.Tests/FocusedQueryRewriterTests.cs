using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class FocusedQueryRewriterTests
{
    private static readonly string[] Excludes = [@"\windows\", @"\node_modules\"];
    private static readonly string[] Exts = ["pdf", "docx"];

    private static string Compose(string query) =>
        FocusedQueryRewriter.Compose(query, Excludes, Exts);

    [Fact]
    public void PlainQuery_GetsExclusionsThenTheWhitelist()
    {
        Assert.Equal(
            @"report !path:""\windows\"" !path:""\node_modules\"" ext:pdf;docx",
            Compose("report"));
    }

    [Fact]
    public void TopLevelOr_AppendsTheSuffixToEveryGroup()
    {
        Assert.Equal(
            @"a !path:""\windows\"" !path:""\node_modules\"" ext:pdf;docx"
            + @" | b !path:""\windows\"" !path:""\node_modules\"" ext:pdf;docx",
            Compose("a | b"));
    }

    [Fact]
    public void QuotedPipe_IsNotASplitPoint()
    {
        Assert.Equal(
            @"""a | b"" !path:""\windows\"" !path:""\node_modules\"" ext:pdf;docx",
            Compose(@"""a | b"""));
    }

    [Theory]
    [InlineData("report ext:xlsx")]
    [InlineData("report EXT:xlsx")] // field names are case-insensitive
    [InlineData("report regex:^r")]
    public void GroupWithAnExplicitTypeFilter_SkipsTheWhitelist(string query)
    {
        Assert.Equal(
            query + @" !path:""\windows\"" !path:""\node_modules\""",
            Compose(query));
    }

    [Theory]
    [InlineData(@"report path:""D:\work""")]
    [InlineData(@"docs\reports")] // a bare backslash already scopes the path
    public void GroupWithAnExplicitLocation_SkipsTheExclusions(string query)
    {
        Assert.Equal(query + " ext:pdf;docx", Compose(query));
    }

    [Fact]
    public void ConflictsAreDecidedPerGroup_NotPerQuery()
    {
        Assert.Equal(
            @"a ext:pdf !path:""\windows\"" !path:""\node_modules\"""
            + @" | b !path:""\windows\"" !path:""\node_modules\"" ext:pdf;docx",
            Compose("a ext:pdf | b"));
    }

    [Theory]
    [InlineData("")]
    [InlineData("   ")]
    public void EmptyQuery_StaysExactlyAsIs(string query)
    {
        // The "no query, no results" rule is the orchestrator's — a rewrite
        // must never turn an empty query into a non-empty one.
        Assert.Equal(query, Compose(query));
    }

    [Fact]
    public void ExcludePathContainingAQuote_IsDropped_OthersStillApply()
    {
        Assert.Equal(
            @"report !path:""\windows\"" ext:pdf",
            FocusedQueryRewriter.Compose(
                "report", [@"bad""path", @"\windows\"], ["pdf"]));
    }

    [Fact]
    public void UnusableExtensionEntries_AreDropped()
    {
        // Whitespace/quotes/pipes would break the unquoted ext: atom;
        // ;/, are the engine's own separators inside one term.
        Assert.Equal(
            "report ext:pdf",
            FocusedQueryRewriter.Compose(
                "report", [], ["a b", "\"x", "c|d", "e;f", " ", "pdf"]));
    }

    [Fact]
    public void NoValidSuffixParts_LeaveTheQueryUntouched()
    {
        // An empty ext: term would match nothing — it must not be emitted.
        Assert.Equal("report", FocusedQueryRewriter.Compose("report", [], []));
        Assert.Equal(
            "report",
            FocusedQueryRewriter.Compose("report", ["  "], ["a b"]));
    }
}
