using FindMyFiles.Highlighting;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Pins the WinUI-free segment splitter that drives the Run rendering.</summary>
public sealed class HighlightSegmenterTests
{
    private static (string Text, bool Hit)[] Split(string text, params (int Start, int Length)[] ranges) =>
        HighlightSegmenter.Split(
                text, ranges.Select(r => new HighlightRange(r.Start, r.Length)).ToList())
            .Select(s => (s.Text, s.Highlighted)).ToArray();

    [Fact]
    public void NoRanges_WholeStringPlain() =>
        Assert.Equal([("hello", false)], Split("hello"));

    [Fact]
    public void MidMatch_SplitsThree() =>
        Assert.Equal([("my", false), ("foo", true), ("file", false)], Split("myfoofile", (2, 3)));

    [Fact]
    public void LeadingMatch_NoEmptyPrefix() =>
        Assert.Equal([("foo", true), ("bar", false)], Split("foobar", (0, 3)));

    [Fact]
    public void TrailingMatch_NoEmptySuffix() =>
        Assert.Equal([("main", false), (".rs", true)], Split("main.rs", (4, 3)));

    [Fact]
    public void MultipleMatches() =>
        Assert.Equal(
            [("a", false), ("bb", true), ("c", false), ("dd", true)],
            Split("abbcdd", (1, 2), (4, 2)));

    [Fact]
    public void OutOfBoundsRange_Clamped() =>
        Assert.Equal([("ab", false), ("c", true)], Split("abc", (2, 99)));

    [Fact]
    public void EntireString_SingleHighlight() =>
        Assert.Equal([("abc", true)], Split("abc", (0, 3)));
}
