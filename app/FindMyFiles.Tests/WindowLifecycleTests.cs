using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class WindowLifecycleTests
{
    [Theory]
    [InlineData(false, false, false, false)]
    [InlineData(false, false, true, false)]
    [InlineData(false, true, true, false)]
    [InlineData(true, true, true, false)]
    [InlineData(true, false, false, false)]
    [InlineData(true, false, true, true)]
    public void ShouldHideToTray_TruthTable(
        bool closeToTray,
        bool explicitExit,
        bool trayAvailable,
        bool expected)
    {
        Assert.Equal(
            expected,
            WindowLifecycle.ShouldHideToTray(closeToTray, explicitExit, trayAvailable));
    }

    [Fact]
    public void TrayRegistration_FailuresStayUnavailable_UntilAConfirmedRetry()
    {
        var registration = new TrayIcon.RegistrationState();

        registration.RecordAttempt(succeeded: false);
        Assert.False(registration.IsAvailable);

        registration.RecordAttempt(succeeded: true);
        Assert.True(registration.IsAvailable);

        registration.MarkUnavailable();
        Assert.False(registration.IsAvailable);
        registration.RecordAttempt(succeeded: false);
        Assert.False(registration.IsAvailable);

        registration.RecordAttempt(succeeded: true);
        Assert.True(registration.IsAvailable);
    }
}
