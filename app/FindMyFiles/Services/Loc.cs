using System.Globalization;
using Microsoft.Windows.ApplicationModel.Resources;

namespace FindMyFiles.Services;

/// <summary>
/// Localized-string facade over the Windows App SDK ResourceLoader (PRI built
/// from Strings/&lt;lang&gt;/Resources.resw). Code keys are flat identifiers
/// (Area_Thing, e.g. Status_Preparing); XAML strings come from x:Uid instead.
/// The <see cref="Override"/> seam lets unit tests resolve keys without a PRI
/// in the test host.
/// </summary>
public static class Loc
{
    /// <summary>Test seam: when set, resolves keys instead of the ResourceLoader.</summary>
    public static Func<string, string>? Override { get; set; }

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
        if (Override is { } over)
        {
            return over(key);
        }

        var value = Loader.GetString(key);
        return string.IsNullOrEmpty(value) ? key : value;
    }

    /// <summary>Resolve a key whose value is a composite format string
    /// (placeholders {0}, {1}, …) and fill it.</summary>
    /// <param name="key">Flat resource identifier whose value is a format string.</param>
    /// <param name="args">Values substituted into the {0}, {1}, … placeholders.</param>
    /// <returns>The localized string with the arguments formatted in.</returns>
    public static string Get(string key, params object[] args) =>
        string.Format(CultureInfo.CurrentCulture, Get(key), args);
}
