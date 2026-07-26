using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class LocTests
{
    [Fact]
    public void GetXamlResolvesDottedReswKeyThroughTestSeam()
    {
        Assert.Equal("Focused", Loc.GetXaml("OptFocused", "Header"));
    }

    [Fact]
    public void GetXamlMakesMissingResourceVisible()
    {
        Assert.Equal("MissingUid.Header", Loc.GetXaml("MissingUid", "Header"));
    }
}
