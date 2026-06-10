using System.Globalization;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ResultRowTests
{
    public ResultRowTests()
    {
        // FormatSize uses N0/N1/N2 — pin the culture for stable expectations.
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    [Theory]
    [InlineData(0UL, "0 B")]
    [InlineData(512UL, "512 B")]
    [InlineData(1023UL, "1023 B")]
    [InlineData(1024UL, "1 KB")]
    [InlineData(10_240UL, "10 KB")]
    [InlineData(1_048_576UL, "1.0 MB")] // 1 MiB
    [InlineData(1_572_864UL, "1.5 MB")] // 1.5 MiB
    [InlineData(3_221_225_472UL, "3.00 GB")] // 3 GiB
    public void Fill_File_FormatsSizeText(ulong size, string expected)
    {
        var row = ResultRow.CreatePlaceholder(0);
        row.Fill(Rows.File(1, "a.txt", size));
        Assert.Equal(expected, row.SizeText);
    }

    [Fact]
    public void Fill_Directory_LeavesSizeTextEmpty()
    {
        var row = ResultRow.CreatePlaceholder(0);
        row.Fill(Rows.Dir(1, "src", size: 4096)); // even with a size, dirs show none
        Assert.Equal(string.Empty, row.SizeText);
    }

    [Fact]
    public void Fill_PositiveMtime_FormatsLocalTimestamp()
    {
        var mtime = new DateTimeOffset(2026, 3, 4, 5, 6, 0, TimeSpan.Zero).ToFileTime();
        var row = ResultRow.CreatePlaceholder(0);
        row.Fill(Rows.File(1, "a.txt", mtime: mtime));
        var expected = DateTimeOffset.FromFileTime(mtime).ToLocalTime().ToString("yyyy/MM/dd HH:mm");
        Assert.Equal(expected, row.DateText);
        Assert.NotEqual(string.Empty, row.DateText);
    }

    [Fact]
    public void Fill_ZeroMtime_LeavesDateTextEmpty()
    {
        var row = ResultRow.CreatePlaceholder(0);
        row.Fill(Rows.File(1, "a.txt", mtime: 0));
        Assert.Equal(string.Empty, row.DateText);
    }

    [Fact]
    public void Fill_PopulatesIdentityAndClearsThePlaceholderFlag()
    {
        var row = ResultRow.CreatePlaceholder(42);
        Assert.True(row.IsPlaceholder);
        Assert.Equal(42, row.Index);

        var data = Rows.File(7, "name.txt", 1);
        row.Fill(data);

        Assert.False(row.IsPlaceholder);
        Assert.Equal(7UL, row.EntryRef);
        Assert.Equal(42, row.Index); // identity survives the fill
        Assert.Equal("name.txt", row.Name);
        Assert.Equal(data.ParentPath, row.ParentPath);
        Assert.Equal(data.FullPath, row.FullPath);
    }
}
