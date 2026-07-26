using System.Globalization;
using Microsoft.Windows.ApplicationModel.Resources;

namespace FindMyFiles.Services;

/// <summary>
/// Localized-string facade over the Windows App SDK ResourceLoader (PRI built
/// from Strings/&lt;lang&gt;/Resources.resw). Code keys are flat identifiers
/// (Area_Thing, e.g. Status_Preparing). <see cref="GetXaml"/> is the explicit
/// bridge for the occasional XAML resource also needed from code.
/// Test-seam builds can override resolution so unit tests do not require a PRI.
/// </summary>
internal static class Loc
{
#if FMF_TEST_SEAMS
    /// <summary>Test seam: when set, resolves keys instead of the ResourceLoader.</summary>
    internal static Func<string, string>? Override { get; set; }
#endif

    // Lazily created: constructing a ResourceLoader needs a PRI, which the
    // (non-WinUI) unit-test host lacks — tests set Override first, so the
    // loader is never touched there.
    private static ResourceLoader? _loader;

    private static ResourceLoader Loader => _loader ??= new ResourceLoader();

    /// <summary>Resolve a key to the current UI language. A missing key falls
    /// back to the key itself so the gap is visible, never an empty UI.</summary>
    /// <param name="key">Flat resource identifier (e.g. Status_Preparing).</param>
    /// <returns>The localized string, or the key itself when unresolved.</returns>
    public static string Get(string key)
    {
#if FMF_TEST_SEAMS
        if (Override is { } over)
        {
            return over(key);
        }
#endif

        var value = Loader.GetString(key);
        return string.IsNullOrEmpty(value) ? key : value;
    }

    /// <summary>Resolve an <c>x:Uid</c> property resource from code. PRI stores
    /// <c>Uid.Property</c> entries as the <c>Uid/Property</c> resource path;
    /// passing the dotted resw name to <see cref="ResourceLoader.GetString(string)"/>
    /// throws <c>NamedResource Not Found</c> instead of returning an empty value.
    /// Keep that representation detail here so call sites cannot repeat it.</summary>
    /// <param name="uid">The XAML <c>x:Uid</c>.</param>
    /// <param name="property">The localized property, such as <c>Header</c>.</param>
    /// <returns>The localized value, or the dotted resw key when unresolved.</returns>
    public static string GetXaml(string uid, string property)
    {
        var dottedKey = $"{uid}.{property}";
#if FMF_TEST_SEAMS
        if (Override is { } over)
        {
            var overridden = over(dottedKey);
            return string.IsNullOrEmpty(overridden) ? dottedKey : overridden;
        }
#endif

        var value = Loader.GetString($"{uid}/{property}");
        return string.IsNullOrEmpty(value) ? dottedKey : value;
    }

    /// <summary>Resolve a key whose value is a composite format string
    /// (placeholders {0}, {1}, …) and fill it.</summary>
    /// <param name="key">Flat resource identifier whose value is a format string.</param>
    /// <param name="args">Values substituted into the {0}, {1}, … placeholders.</param>
    /// <returns>The localized string with the arguments formatted in.</returns>
    public static string Get(string key, params object[] args) =>
        string.Format(CultureInfo.CurrentCulture, Get(key), args);
}
