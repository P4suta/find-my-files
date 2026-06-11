using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FindMyFiles.Services;

namespace FindMyFiles.Converters;

/// <summary>
/// Pure x:Bind function-binding converters — no IValueConverter plumbing,
/// x:Bind calls these statically and stays typed end to end.
/// </summary>
public static class UiConverters
{
    public static Visibility BoolToVis(bool value) =>
        value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>App severity → InfoBar severity (the InfoBar enum is the
    /// view's vocabulary; the app's is <see cref="NotifySeverity"/>).</summary>
    public static InfoBarSeverity ToInfoSeverity(NotifySeverity s) => s switch
    {
        NotifySeverity.Error => InfoBarSeverity.Error,
        NotifySeverity.Warning => InfoBarSeverity.Warning,
        _ => InfoBarSeverity.Informational,
    };
}
