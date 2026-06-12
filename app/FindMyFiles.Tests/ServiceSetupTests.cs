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

            // Dev tree: engine\target\release above the bin dir.
            var dev = Path.Combine(root.FullName, "engine", "target", "release");
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
}
