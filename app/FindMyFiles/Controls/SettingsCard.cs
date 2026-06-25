using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Controls;

/// <summary>
/// A minimal, dependency-free Fluent "settings card": a localizable header
/// label on the left and the card's single <see cref="ContentControl.Content"/>
/// (one control) right-aligned, on the same card chrome the diagnostics panel
/// uses. It stands in for CommunityToolkit's <c>SettingsCard</c>, which cannot
/// be referenced here — the toolkit pulls the WindowsAppSDK 1.x metapackage and
/// hard-conflicts with this app's slim 2.x component packages. Apply
/// <c>SettingsCardStyle</c> (Themes/Settings.xaml) and set <see cref="Header"/>
/// via x:Uid (resw key "&lt;Uid&gt;.Header").
/// </summary>
public sealed class SettingsCard : ContentControl
{
    /// <summary>Identifies the <see cref="Header"/> dependency property.</summary>
    public static readonly DependencyProperty HeaderProperty =
        DependencyProperty.Register(
            nameof(Header),
            typeof(string),
            typeof(SettingsCard),
            new PropertyMetadata(string.Empty));

    /// <summary>The setting's label, shown to the left of the control.</summary>
    public string Header
    {
        get => (string)GetValue(HeaderProperty);
        set => SetValue(HeaderProperty, value);
    }
}
