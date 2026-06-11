using FindMyFiles.Converters;
using FindMyFiles.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class UiConvertersTests
{
    [Fact]
    public void Bool_maps_to_visibility()
    {
        Assert.Equal(Visibility.Visible, UiConverters.BoolToVis(true));
        Assert.Equal(Visibility.Collapsed, UiConverters.BoolToVis(false));
    }

    [Theory]
    [InlineData(NotifySeverity.Error, InfoBarSeverity.Error)]
    [InlineData(NotifySeverity.Warning, InfoBarSeverity.Warning)]
    [InlineData(NotifySeverity.Info, InfoBarSeverity.Informational)]
    public void App_severity_maps_to_infobar_severity(NotifySeverity s, InfoBarSeverity expected) =>
        Assert.Equal(expected, UiConverters.ToInfoSeverity(s));
}
