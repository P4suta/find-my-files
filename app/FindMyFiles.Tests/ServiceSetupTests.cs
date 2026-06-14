using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class ServiceSetupTests
{
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
            File.WriteAllText(devExe, "");
            Assert.Equal(devExe, ServiceSetup.LocateServiceExe(baseDir));

            // The dist bundle wins over the dev tree.
            var bundled = Path.Combine(baseDir, "fmf-service.exe");
            File.WriteAllText(bundled, "");
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
        Assert.StartsWith("S-1-", sid);
        Assert.True(ServiceSetup.IsValidSid(sid), "own SID must survive the injection guard");
    }
}
