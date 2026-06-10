using System.Globalization;
using FindMyFiles.Engine;
using FindMyFiles.ViewModels;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class StatusFormatterTests
{
    public StatusFormatterTests()
    {
        // N0/F1 are culture-sensitive; pin the culture so the expected
        // strings hold on any machine. CurrentCulture flows across awaits
        // and is per-thread, so this cannot leak into parallel tests.
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    [Fact]
    public void Count_WithTrace_ShowsMillisecondsAndHits()
    {
        var trace = new QueryTraceData { TotalUs = 12_345 };
        Assert.Equal("12.3 ms · 1,234 件", StatusFormatter.Count(trace, 1234));
    }

    [Fact]
    public void Count_WithoutTrace_ShowsHitsOnly()
    {
        Assert.Equal("1,000,000 件", StatusFormatter.Count(null, 1_000_000));
    }

    [Fact]
    public void QueryError_PrefixesTheMessage()
    {
        Assert.Equal("クエリエラー: unbalanced quote", StatusFormatter.QueryError("unbalanced quote"));
    }

    [Fact]
    public void IndexingStarted_NoVolumes_ReportsNoneFound()
    {
        Assert.Equal("NTFS固定ドライブが見つかりません", StatusFormatter.IndexingStarted([]));
    }

    [Fact]
    public void IndexingStarted_MultipleVolumes_ListsThem()
    {
        Assert.Equal("インデックス作成中: C:, D:", StatusFormatter.IndexingStarted(["C:", "D:"]));
    }

    [Theory]
    [InlineData(VolumeState.Scanning, "C: をインデックス中… 1,000 件")]
    [InlineData(VolumeState.Ready, "C: 準備完了 — 1,000 件")]
    [InlineData(VolumeState.Rescanning, "C: を再スキャン中…")]
    [InlineData(VolumeState.Failed, "C: のインデックスに失敗")]
    public void Volume_KnownStates_FormatTheStatusLine(VolumeState state, string expected)
    {
        var status = new VolumeStatus("C:", state, 1000);
        Assert.Equal(expected, StatusFormatter.Volume(status, "前のテキスト"));
    }

    [Fact]
    public void Volume_UnknownState_FallsBackToTheCurrentText()
    {
        var status = new VolumeStatus("C:", (VolumeState)99, 0);
        Assert.Equal("前のテキスト", StatusFormatter.Volume(status, "前のテキスト"));
    }
}
