using System.Globalization;
using FindMyFiles.Engine;
using FindMyFiles.Tests.TestDoubles;
using FindMyFiles.ViewModels;
using Xunit;
using static FindMyFiles.Tests.TestDoubles.Polling;

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

    // Assertions are against the en-US resource values (LocTestInit pins them).
    [Fact]
    public void Count_WithTrace_ShowsMillisecondsAndHits()
    {
        var trace = new QueryTraceData { TotalUs = 12_345 };
        Assert.Equal("12.3 ms · 1,234 items", StatusFormatter.Count(trace, 1234));
    }

    [Fact]
    public void Count_WithoutTrace_ShowsHitsOnly()
    {
        Assert.Equal("1,000,000 items", StatusFormatter.Count(null, 1_000_000));
    }

    [Fact]
    public void QueryError_PrefixesTheMessage()
    {
        Assert.Equal("Query error: unbalanced quote", StatusFormatter.QueryError("unbalanced quote"));
    }

    [Fact]
    public void Overall_NoStatusNoDrives_ReportsNoneFound()
    {
        Assert.Equal("No fixed NTFS drives found", StatusFormatter.Overall([], []));
    }

    [Fact]
    public void Overall_NoStatusButRequested_ReportsIndexing()
    {
        // The engine hasn't surfaced status yet, but we asked to index them.
        Assert.Equal("Indexing: C:, D:", StatusFormatter.Overall([], ["C:", "D:"]));
    }

    [Fact]
    public void Overall_AllReady_ReportsReadyWithTotal()
    {
        VolumeStatus[] vols =
        [
            new("C:", VolumeState.Ready, 1000),
            new("D:", VolumeState.Ready, 234),
        ];
        Assert.Equal("Ready — 1,234 items", StatusFormatter.Overall(vols, ["C:", "D:"]));
    }

    [Fact]
    public void Overall_SomeScanning_ListsOnlyPendingVolumes()
    {
        VolumeStatus[] vols =
        [
            new("C:", VolumeState.Ready, 1000),
            new("D:", VolumeState.Scanning, 0),
        ];
        Assert.Equal("Indexing: D:", StatusFormatter.Overall(vols, ["C:", "D:"]));
    }

    [Fact]
    public void Overall_AllFailed_ReportsFailure()
    {
        VolumeStatus[] vols = [new("C:", VolumeState.Failed, 0)];
        Assert.Equal("Indexing failed: C:", StatusFormatter.Overall(vols, ["C:"]));
    }

    [Theory]
    [InlineData(VolumeState.Scanning, "C: indexing… 1,000 items")]
    [InlineData(VolumeState.Ready, "C: ready — 1,000 items")]
    [InlineData(VolumeState.Rescanning, "C: rescanning…")]
    [InlineData(VolumeState.Failed, "C: indexing failed")]
    public void Volume_KnownStates_FormatTheStatusLine(VolumeState state, string expected)
    {
        var status = new VolumeStatus("C:", state, 1000);
        Assert.Equal(expected, StatusFormatter.Volume(status, "prev"));
    }

    [Fact]
    public void Volume_UnknownState_FallsBackToTheCurrentText()
    {
        var status = new VolumeStatus("C:", (VolumeState)99, 0);
        Assert.Equal("prev", StatusFormatter.Volume(status, "prev"));
    }

    [Fact]
    public void EngineMode_FakeClient_SaysDemo()
    {
        using var fake = new FakeEngineClient();
        Assert.Equal("demo", StatusFormatter.EngineMode(fake));
    }

    [Fact]
    public void EngineMode_PipeBeforeFirstConnect_SaysConnecting()
    {
        using var pipe = new PipeEngineClient(
            "fmf-test-mode-" + Guid.NewGuid().ToString("N"), autoStart: false);
        Assert.Equal("connecting…", StatusFormatter.EngineMode(pipe));
    }

    [Fact]
    public async Task EngineMode_PipeConnected_SaysServiceConnection()
    {
        using var server = new FakePipeServer();
        using var client = new PipeEngineClient(server.PipeName);
        await WaitUntilAsync(
            () => client.Connection == EngineConnectionState.Connected, "connected");
        Assert.Equal("service connected", StatusFormatter.EngineMode(client));
    }

    [Fact]
    public void EngineMode_UnknownClientType_IsEmpty()
    {
        using var stub = new StubEngineClient();
        Assert.Equal(string.Empty, StatusFormatter.EngineMode(stub));
    }
}
