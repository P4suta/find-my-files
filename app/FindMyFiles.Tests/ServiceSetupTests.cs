using FindMyFiles.Engine;
using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ServiceSetupTests
{
    [Theory]
    [InlineData("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", true)]
    [InlineData("0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF", false)]
    [InlineData("0123456789abcdef", false)]
    [InlineData("UNPINNED", false)]
    [InlineData(null, false)]
    public void ServiceImageDigest_requires_exact_lowercase_sha256(
        string? digest,
        bool expected) =>
        Assert.Equal(expected, ServiceExecutableTrust.IsPinnedDigest(digest));

    [Fact]
    public void Ordinary_test_build_cannot_cross_the_elevation_boundary()
    {
        Assert.Equal("UNPINNED", ServiceExecutableTrust.ExpectedImageSha256);
        Assert.Throws<System.Security.SecurityException>(
            () => ServiceExecutableTrust.Acquire("fmf-service.exe"));
    }

    [Fact]
    public void ServiceProtocolMarker_accepts_only_the_generated_exact_value()
    {
        Assert.True(
            ServiceSetup.IsServiceProtocolMarkerCompatible(
                EngineContract.ServiceProtocolMarker));
        Assert.False(ServiceSetup.IsServiceProtocolMarkerCompatible(null));
        Assert.False(ServiceSetup.IsServiceProtocolMarkerCompatible(string.Empty));
        Assert.False(
            ServiceSetup.IsServiceProtocolMarkerCompatible(
                EngineContract.ServiceProtocolMarker + " "));
        Assert.False(
            ServiceSetup.IsServiceProtocolMarkerCompatible(
                EngineContract.ServiceProtocolMarker.Replace(
                    $"protocol={EngineContract.ProtocolVersion}",
                    $"protocol={EngineContract.ProtocolVersion + 1}",
                    StringComparison.Ordinal)));
    }

    [Theory]
    [InlineData(1u, true)]
    [InlineData(2u, false)] // START_PENDING
    [InlineData(3u, false)] // STOP_PENDING
    [InlineData(4u, false)]
    [InlineData(5u, false)] // CONTINUE_PENDING
    [InlineData(6u, false)] // PAUSE_PENDING
    [InlineData(7u, false)] // PAUSED
    [InlineData(0u, false)] // malformed: fail closed
    public void MapServiceState_only_treats_SERVICE_STOPPED_as_stopped(
        uint raw,
        bool expectedStopped) =>
        Assert.Equal(
            expectedStopped ? EngineServiceState.Stopped : EngineServiceState.Running,
            ServiceSetup.MapServiceState(raw));

    [Fact]
    public void LocateServiceExe_PrefersBundled_ThenDevTree_ElseNull()
    {
        var root = Directory.CreateTempSubdirectory("fmf-setup-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "app", "bin");
            Directory.CreateDirectory(baseDir);
            Assert.Null(ServiceSetup.LocateServiceExe(baseDir));

            // Dev tree: build\engine\release above the bin dir.
            var dev = Path.Combine(root.FullName, "build", "engine", "release");
            Directory.CreateDirectory(dev);
            var devExe = Path.Combine(dev, "fmf-service.exe");
            File.WriteAllText(devExe, string.Empty);
            Assert.Equal(devExe, ServiceSetup.LocateServiceExe(baseDir));

            // The dist bundle wins over the dev tree.
            var bundled = Path.Combine(baseDir, "fmf-service.exe");
            File.WriteAllText(bundled, string.Empty);
            Assert.Equal(bundled, ServiceSetup.LocateServiceExe(baseDir));
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Theory]
    [InlineData("S-1-5-21-1654600493-3733564142-2704359447-1001", true)]
    [InlineData("S-1-5-18", true)] // well-formed (validate_user_sid rejects it server-side)
    [InlineData(null, false)]
    [InlineData("", false)]
    [InlineData("not-a-sid", false)]
    [InlineData("S-1-5-21-abc", false)]
    [InlineData("S-1-05-18", false)]
    [InlineData("S-2-5-18", false)]
    [InlineData("S-1-281474976710656-18", false)]
    [InlineData("S-1-5-4294967296", false)]
    [InlineData("S-1-5-21-1; rm -rf", false)] // ; and space — injection attempt
    [InlineData("S-1-5-21-1 --owner-sid=evil", false)] // space would split into args
    [InlineData("S-1-5-21-１", false)] // full-width digit is not ASCII
    public void IsValidSid_AcceptsWellFormed_RejectsInjection(string? input, bool expected)
    {
        Assert.Equal(expected, ServiceSetup.IsValidSid(input));
    }

    [Fact]
    public void CurrentUserSid_ReturnsForwardableSid()
    {
        var sid = ServiceSetup.CurrentUserSid();
        Assert.NotNull(sid);
        Assert.StartsWith("S-1-", sid, StringComparison.Ordinal);
        Assert.True(ServiceSetup.IsValidSid(sid), "own SID must survive the injection guard");
    }

    [Fact]
    public void PollForCompatibleStartedService_WaitsForRunningThenCurrentPipe()
    {
        var pids = new Queue<uint>([0, 0, 42]);
        var probes = new Queue<bool>([false, true]);
        var waits = 0;

        var compatible = ServiceSetup.PollForCompatibleStartedService(
            () => pids.Dequeue(),
            () => probes.Dequeue(),
            startPollAttempts: 3,
            compatibilityProbeAttempts: 2,
            () => waits++);

        Assert.True(compatible);
        Assert.Equal(3, waits); // two START_PENDING polls + one probe grace
        Assert.Empty(pids);
        Assert.Empty(probes);
    }

    [Fact]
    public void PollForCompatibleStartedService_RejectsObsoletePipe()
    {
        var probes = 0;
        var compatible = ServiceSetup.PollForCompatibleStartedService(
            () => 42,
            () =>
            {
                probes++;
                return false;
            },
            startPollAttempts: 10,
            compatibilityProbeAttempts: 3,
            () => { });

        Assert.False(compatible);
        Assert.Equal(3, probes);
    }

    [Fact]
    public void PollForCompatibleStartedService_DoesNotProbeBeforeRunning()
    {
        var probes = 0;
        var compatible = ServiceSetup.PollForCompatibleStartedService(
            () => 0,
            () =>
            {
                probes++;
                return true;
            },
            startPollAttempts: 3,
            compatibilityProbeAttempts: 2,
            () => { });

        Assert.False(compatible);
        Assert.Equal(0, probes);
    }
}
