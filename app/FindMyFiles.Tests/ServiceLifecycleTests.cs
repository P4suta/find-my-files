using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// Defense-in-depth behavior tests for the pure, unelevated
/// <see cref="ServiceSetup"/> surfaces that the thicker suites skipped: the
/// exe-location walk and the SID injection guard that gate the one-time
/// elevation. Anything touching the SCM, elevation, or a real process launch is
/// deliberately out of scope here.
/// </summary>
public sealed class ServiceLifecycleTests
{
    [Fact]
    public void Locate_service_exe_returns_null_when_nothing_is_present()
    {
        // The simplest transition: an isolated bin dir with no bundle and no dev
        // tree above it resolves to null, so the caller falls back to setup.
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
        // The dev-tree probe walks up from the bin dir; a deeply nested bin dir
        // must still find build\engine\release sitting near the repo root.
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
        // A bundle next to the bin dir short-circuits the dev-tree walk entirely.
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
    [InlineData("S-1-5")] // minimal well-formed prefix
    [InlineData("S-1-5-21-abc")] // letters allowed — it's an injection guard, not a strict parser
    public void Is_valid_sid_accepts_injection_safe_values(string input)
    {
        Assert.True(ServiceSetup.IsValidSid(input));
    }

    [Theory]
    [InlineData("s-1-5-18")] // lowercase prefix — the S-1- check is case-sensitive
    [InlineData("S-1-5-21-1\n--owner-sid=evil")] // newline injection
    [InlineData("S-1-5-21-1\t--owner-sid=evil")] // tab injection
    [InlineData("X-1-5-18")] // wrong authority prefix
    public void Is_valid_sid_rejects_malformed_or_injecting_values(string input)
    {
        Assert.False(ServiceSetup.IsValidSid(input));
    }
}
