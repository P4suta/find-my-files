using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class WindowLifecycleTests
{
    // ADR-0030: a close (×) hides only when the setting is on, the exit is not
    // explicit, and Explorer confirms that a real tray icon exists.
    [Theory]
    [InlineData(false, false, false, false)] // off, no icon → exit
    [InlineData(false, false, true, false)] // off, icon → exit
    [InlineData(false, true, true, false)] // off, explicit exit → exit
    [InlineData(true, true, true, false)] // on, explicit exit → exit
    [InlineData(true, false, false, false)] // on, missing icon → exit
    [InlineData(true, false, true, true)] // on, real icon → hide
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

        registration.RecordAttempt(succeeded: false); // initial NIM_ADD failure
        Assert.False(registration.IsAvailable);

        registration.RecordAttempt(succeeded: true);
        Assert.True(registration.IsAvailable);

        registration.MarkUnavailable(); // TaskbarCreated: Explorer lost it
        Assert.False(registration.IsAvailable);
        registration.RecordAttempt(succeeded: false); // re-add failure
        Assert.False(registration.IsAvailable);

        registration.RecordAttempt(succeeded: true); // later re-add succeeds
        Assert.True(registration.IsAvailable);
    }
}
