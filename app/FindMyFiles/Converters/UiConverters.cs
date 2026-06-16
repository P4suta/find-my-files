using FindMyFiles.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Converters;

/// <summary>
/// Pure x:Bind function-binding converters — no IValueConverter plumbing,
/// x:Bind calls these statically and stays typed end to end.
/// </summary>
public static class UiConverters
{
    /// <summary>Maps <c>true</c> to <c>Visible</c> and <c>false</c> to <c>Collapsed</c>
    /// (negate on the bind side where inversion is needed).</summary>
    /// <param name="value">The bound boolean to map to a visibility.</param>
    /// <returns><c>Visible</c> when <paramref name="value"/> is true, otherwise <c>Collapsed</c>.</returns>
    public static Visibility BoolToVis(bool value) =>
        value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary><c>Visible</c> when the string is non-null and non-empty, otherwise <c>Collapsed</c>.
    /// Used to show an element only when a value exists (e.g. error text).</summary>
    /// <param name="value">The bound string whose presence drives visibility.</param>
    /// <returns><c>Visible</c> when <paramref name="value"/> is non-null and non-empty, otherwise <c>Collapsed</c>.</returns>
    public static Visibility VisibleIfNotEmpty(string? value) =>
        string.IsNullOrEmpty(value) ? Visibility.Collapsed : Visibility.Visible;

    /// <summary>App severity → InfoBar severity (the InfoBar enum is the
    /// view's vocabulary; the app's is <see cref="NotifySeverity"/>).</summary>
    /// <param name="s">The app-side severity to translate.</param>
    /// <returns>The matching <see cref="InfoBarSeverity"/> for the view.</returns>
    public static InfoBarSeverity ToInfoSeverity(NotifySeverity s) => s switch
    {
        NotifySeverity.Error => InfoBarSeverity.Error,
        NotifySeverity.Warning => InfoBarSeverity.Warning,
        _ => InfoBarSeverity.Informational,
    };
}
