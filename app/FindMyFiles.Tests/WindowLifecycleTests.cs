using FindMyFiles.Services;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class WindowLifecycleTests
{
    // ADR-0030: a close (×) hides to tray only when the setting is on AND the
    // exit is not an explicit tray-menu "Exit".
    [Theory]
    [InlineData(false, false, false)] // off, normal close → exit
    [InlineData(false, true, false)] // off, explicit exit → exit
    [InlineData(true, false, true)] // on, normal close → hide to tray
    [InlineData(true, true, false)] // on, explicit exit → exit
    public void ShouldHideToTray_TruthTable(bool closeToTray, bool explicitExit, bool expected)
    {
        Assert.Equal(expected, WindowLifecycle.ShouldHideToTray(closeToTray, explicitExit));
    }
}
