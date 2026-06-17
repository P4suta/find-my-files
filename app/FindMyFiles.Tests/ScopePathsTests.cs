using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Unit tests for the pure scope path helpers (ADR-0024/-0025):
/// nested-root normalization and the exclude under-root guard.</summary>
public sealed class ScopePathsTests
{
    // CA1861: constant array arguments are hoisted to fields.
    private static readonly string[] NestedPair = [@"C:\A", @"C:\A\B"];
    private static readonly string[] ParentAfterChild = [@"C:\A\B", @"C:\Other", @"C:\A"];
    private static readonly string[] DupeCase = [@"C:\A", @"c:\a"];
    private static readonly string[] SiblingLookalike = [@"C:\A", @"C:\AB"];
    private static readonly string[] DriveRoot = [@"C:\"];
    private static readonly string[] RootA = [@"C:\A"];
    private static readonly string[] OnlyA = [@"C:\A"];
    private static readonly string[] OtherThenA = [@"C:\Other", @"C:\A"];

    [Fact]
    public void Normalize_collapses_a_nested_child() =>
        Assert.Equal(OnlyA, ScopePaths.Normalize(NestedPair));

    [Fact]
    public void Normalize_collapses_when_parent_comes_after_child() =>
        Assert.Equal(OtherThenA, ScopePaths.Normalize(ParentAfterChild));

    [Fact]
    public void Normalize_drops_case_insensitive_duplicates() =>
        Assert.Equal(OnlyA, ScopePaths.Normalize(DupeCase));

    [Fact]
    public void Normalize_keeps_sibling_prefix_lookalikes() =>
        Assert.Equal(SiblingLookalike, ScopePaths.Normalize(SiblingLookalike));

    [Fact]
    public void Normalize_preserves_a_drive_root_trailing_separator() =>
        Assert.Equal(DriveRoot, ScopePaths.Normalize(DriveRoot));

    [Theory]
    [InlineData(@"C:\A\B", true)]
    [InlineData(@"c:\a\b\c", true)] // case-insensitive
    [InlineData(@"C:\AB", false)] // sibling lookalike, separator-aware
    [InlineData(@"C:\A", false)] // equal to a root, not strictly under
    [InlineData(@"D:\A\B", false)] // different root
    public void IsUnderAnyRoot_is_separator_aware_and_strict(string path, bool expected) =>
        Assert.Equal(expected, ScopePaths.IsUnderAnyRoot(path, RootA));
}
