using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary> Defense-in-depth behavior tests for the pure, unelevated <see cref="ServiceSetup"/> surfaces that the thicker suites skipped: the exe-location walk and the SID injection guard that gate the one-time elevation.
/// Anything touching the SCM, elevation, or a real process launch is deliberately out of scope here.
/// </summary>
public sealed class ServiceLifecycleTests
{
    [Fact]
    public void Locate_service_exe_returns_null_when_nothing_is_present()
    {
        var root = Directory.CreateTempSubdirectory("fmf-lifecycle-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "app", "bin");
            Directory.CreateDirectory(baseDir);

            Assert.Null(ServiceSetup.LocateServiceExe(baseDir));
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Fact]
    public void Locate_service_exe_walks_up_several_levels_to_find_the_dev_tree()
    {
        var root = Directory.CreateTempSubdirectory("fmf-lifecycle-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "a", "b", "c", "bin");
            Directory.CreateDirectory(baseDir);

            var dev = Path.Combine(root.FullName, "build", "engine", "release");
            Directory.CreateDirectory(dev);
            var devExe = Path.Combine(dev, "fmf-service.exe");
            File.WriteAllText(devExe, string.Empty);

            Assert.Equal(devExe, ServiceSetup.LocateServiceExe(baseDir));
        }
        finally
        {
            root.Delete(recursive: true);
        }
    }

    [Fact]
    public void Locate_service_exe_returns_the_bundle_without_walking_when_present()
    {
        var root = Directory.CreateTempSubdirectory("fmf-lifecycle-test");
        try
        {
            var baseDir = Path.Combine(root.FullName, "app", "bin");
            Directory.CreateDirectory(baseDir);
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
    [InlineData("S-1-5")]
    [InlineData("S-1-5-21-1654600493-3733564142-2704359447-1001")]
    public void Is_valid_sid_accepts_canonical_values(string input)
    {
        Assert.True(ServiceSetup.IsValidSid(input));
    }

    [Theory]
    [InlineData("s-1-5-18")]
    [InlineData("S-1-5-21-abc")]
    [InlineData("S-1-05-18")]
    [InlineData("S-2-5-18")]
    [InlineData("S-1-281474976710656-18")]
    [InlineData("S-1-5-4294967296")]
    [InlineData("S-1-5-21-1\n--owner-sid=evil")]
    [InlineData("S-1-5-21-1\t--owner-sid=evil")]
    [InlineData("X-1-5-18")]
    public void Is_valid_sid_rejects_malformed_or_injecting_values(string input)
    {
        Assert.False(ServiceSetup.IsValidSid(input));
    }
}
