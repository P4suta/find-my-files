using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>
/// The test host runs UNPACKAGED (no MSIX identity), so these pin the unpackaged
/// branch that the portable zip build also takes: identity absent, install path
/// null, and — critically — the App Model probe must NOT throw when there is no
/// package (the reason detection goes through GetCurrentPackageFullName rather
/// than Package.Current, which throws unpackaged). ADR-0028.
/// </summary>
public sealed class PackageIdentityTests
{
    [Fact]
    public void Unpackaged_host_reports_no_identity()
    {
        Assert.False(PackageIdentity.IsPackaged);
    }

    [Fact]
    public void Unpackaged_host_has_no_install_location()
    {
        Assert.Null(PackageIdentity.InstalledLocationPath);
    }

    [Fact]
    public void Detection_is_stable_across_calls()
    {
        // Cached for the process lifetime — a second read must agree with the first.
        Assert.Equal(PackageIdentity.IsPackaged, PackageIdentity.IsPackaged);
    }
}
