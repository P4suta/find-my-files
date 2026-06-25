using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FindMyFiles.Controls;

/// <summary>
/// A minimal, dependency-free Fluent "settings card": an optional leading icon,
/// a header with an optional description on the left, and the card's single
/// <see cref="ContentControl.Content"/> (one control) right-aligned, on the same
/// card chrome the diagnostics panel uses. It stands in for CommunityToolkit's
/// <c>SettingsCard</c>, which cannot be referenced here — the toolkit pulls the
/// WindowsAppSDK 1.x metapackage and hard-conflicts with this app's slim 2.x
/// component packages. Apply <c>SettingsCardStyle</c> (Themes/Settings.xaml);
/// localize <see cref="Header"/> / <see cref="Description"/> via x:Uid (resw keys
/// "&lt;Uid&gt;.Header" / "&lt;Uid&gt;.Description"). The icon and the
/// description each collapse when unset.
/// </summary>
[TemplatePart(Name = PartIcon, Type = typeof(FrameworkElement))]
[TemplatePart(Name = PartDescription, Type = typeof(FrameworkElement))]
public sealed class SettingsCard : ContentControl
{
    private const string PartIcon = "PART_Icon";
    private const string PartDescription = "PART_Description";

    /// <summary>Identifies the <see cref="Header"/> dependency property.</summary>
    public static readonly DependencyProperty HeaderProperty =
        DependencyProperty.Register(
            nameof(Header),
            typeof(string),
            typeof(SettingsCard),
            new PropertyMetadata(string.Empty));

    /// <summary>Identifies the <see cref="Description"/> dependency property.</summary>
    public static readonly DependencyProperty DescriptionProperty =
        DependencyProperty.Register(
            nameof(Description),
            typeof(string),
            typeof(SettingsCard),
            new PropertyMetadata(null, OnOptionalChanged));

    /// <summary>Identifies the <see cref="Glyph"/> dependency property.</summary>
    public static readonly DependencyProperty GlyphProperty =
        DependencyProperty.Register(
            nameof(Glyph),
            typeof(string),
            typeof(SettingsCard),
            new PropertyMetadata(null, OnOptionalChanged));

    /// <summary>The setting's label, shown to the left of the control.</summary>
    public string Header
    {
        get => (string)GetValue(HeaderProperty);
        set => SetValue(HeaderProperty, value);
    }

    /// <summary>Optional secondary line under the header; it collapses when unset.</summary>
    public string? Description
    {
        get => (string?)GetValue(DescriptionProperty);
        set => SetValue(DescriptionProperty, value);
    }

    /// <summary>Optional leading Segoe Fluent glyph; the icon collapses when unset.</summary>
    public string? Glyph
    {
        get => (string?)GetValue(GlyphProperty);
        set => SetValue(GlyphProperty, value);
    }

    private FrameworkElement? _iconPart;
    private FrameworkElement? _descriptionPart;

    /// <summary>Caches the optional template parts (icon, description) and sets
    /// their visibility from the current <see cref="Glyph"/> / <see cref="Description"/>.</summary>
    protected override void OnApplyTemplate()
    {
        base.OnApplyTemplate();
        _iconPart = GetTemplateChild(PartIcon) as FrameworkElement;
        _descriptionPart = GetTemplateChild(PartDescription) as FrameworkElement;
        UpdateOptionalParts();
    }

    private static void OnOptionalChanged(DependencyObject d, DependencyPropertyChangedEventArgs e) =>
        ((SettingsCard)d).UpdateOptionalParts();

    private void UpdateOptionalParts()
    {
        if (_iconPart is not null)
        {
            _iconPart.Visibility = string.IsNullOrEmpty(Glyph) ? Visibility.Collapsed : Visibility.Visible;
        }

        if (_descriptionPart is not null)
        {
            _descriptionPart.Visibility = string.IsNullOrEmpty(Description) ? Visibility.Collapsed : Visibility.Visible;
        }
    }
}
