using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class BuildInfoTests
{
    [Theory]
    [InlineData("0.1.0", "0.1.0")]
    [InlineData("0.1.0-dev+g7346c64", "0.1.0")]
    [InlineData("0.1.0-nightly.20260629+g7346c64", "0.1.0")]
    [InlineData("1.2.3+gabc1234", "1.2.3")]
    [InlineData("", "")]
    public void BaseOf_strips_prerelease_and_metadata(string version, string expected) =>
        Assert.Equal(expected, BuildInfo.BaseOf(version));

    [Theory]
    [InlineData("0.1.0-dev+g7346c64", "0.1.0-nightly.20260629+gabc1234", true)]
    [InlineData("0.1.0", "0.1.0+gabc1234", true)]
    [InlineData("0.1.0-dev+g7346c64", "0.2.0-dev+g7346c64", false)]
    [InlineData("0.1.0", "1.0.0", false)]
    public void SameBase_compares_only_the_triple(string a, string b, bool expected) =>
        Assert.Equal(expected, BuildInfo.SameBase(a, b));
}
