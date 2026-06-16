using System.Runtime.CompilerServices;
using System.Xml.Linq;
using FindMyFiles.Services;

namespace FindMyFiles.Tests;

/// <summary>
/// Points <see cref="Loc"/> at the en-US resource values for the whole test
/// run, so localized code (e.g. <see cref="FindMyFiles.ViewModels.StatusFormatter"/>)
/// resolves deterministically without a PRI in the non-WinUI test host.
/// </summary>
internal static class LocTestInit
{
    [ModuleInitializer]
    public static void Init()
    {
        var path = Path.Combine(AppContext.BaseDirectory, "Strings.en-US.resw");
        var map = XDocument.Load(path).Root!
            .Elements("data")
            .ToDictionary(d => d.Attribute("name")!.Value, d => d.Element("value")!.Value, StringComparer.Ordinal);
        Loc.Override = key => map.TryGetValue(key, out var v) ? v : key;
    }
}
