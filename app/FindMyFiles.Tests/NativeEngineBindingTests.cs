using System.Reflection;
using System.Runtime.InteropServices;
using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

/// <summary>Guards the FFI binding surface: every P/Invoke entry point in
/// <see cref="NativeEngine"/> must name an export that actually exists in
/// fmf_engine.dll. The DLL's Rust exports are all lowercase snake_case
/// (<c>#[no_mangle] extern "C" fn fmf_*</c>), and <see cref="LibraryImportAttribute"/>
/// resolves the method name as the symbol through the case-sensitive
/// GetProcAddress — so a single PascalCased name (e.g. an analyzer renaming
/// <c>fmf_set_event_callback</c> to <c>Fmf_set_event_callback</c>) silently
/// breaks in-proc / scope mode with <see cref="EntryPointNotFoundException"/>,
/// invisible to the fake-backed suite which never loads the DLL. Pinning the
/// entry-point shape here makes such drift fail the build, not a user's search.</summary>
public sealed class NativeEngineBindingTests
{
    [Fact]
    public void Every_fmf_engine_entry_point_is_lowercase_snake_case()
    {
        var entryPoints = typeof(NativeEngine)
            .GetMethods(BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic)
            .Select(m => (m.Name, Import: m.GetCustomAttribute<LibraryImportAttribute>()))
            .Where(x => x.Import is { LibraryName: "fmf_engine" })
            .Select(x => x.Import!.EntryPoint ?? x.Name)
            .ToList();

        // Guard against a false green: if the attribute ever stops surfacing
        // through reflection, finding nothing must fail rather than pass vacuously.
        Assert.True(
            entryPoints.Count >= 10,
            $"expected NativeEngine's P/Invoke surface, found only {entryPoints.Count} entry points");

        var bad = entryPoints.Where(name => !IsLowerSnakeFmf(name)).ToList();
        var detail = "fmf_engine entry points must match the DLL's lowercase exports "
            + "(GetProcAddress is case-sensitive): " + string.Join(", ", bad);
        Assert.True(bad.Count == 0, detail);
    }

    private static bool IsLowerSnakeFmf(string name) =>
        name.StartsWith("fmf_", StringComparison.Ordinal)
        && name.All(c => c is (>= 'a' and <= 'z') or (>= '0' and <= '9') or '_');
}
