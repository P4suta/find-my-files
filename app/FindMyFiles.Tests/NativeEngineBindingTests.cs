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
/// breaks in-proc mode with <see cref="EntryPointNotFoundException"/>,
/// invisible to the fake-backed suite which never loads the DLL. Pinning the
/// entry-point shape here makes such drift fail the build, not a user's search.</summary>
public sealed class NativeEngineBindingTests
{
    [Fact]
    public void Ffi_client_rejects_an_incompatible_dll_before_structured_calls()
    {
        FfiEngineClient.EnsureCompatibleAbi(EngineContract.AbiVersion);

        var ex = Assert.Throws<EngineUnavailableException>(
            () => FfiEngineClient.EnsureCompatibleAbi(EngineContract.AbiVersion + 1));

        Assert.Contains(
            $"expects {EngineContract.AbiVersion}",
            ex.Message,
            StringComparison.Ordinal);
        Assert.Contains(
            $"reports {EngineContract.AbiVersion + 1}",
            ex.Message,
            StringComparison.Ordinal);
    }

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

    [Theory]
    [InlineData("fmf_blob_free")]
    [InlineData("fmf_page_free")]
    public void Allocation_free_exports_take_owner_ids_only(string methodName)
    {
        var method = typeof(NativeEngine).GetMethod(
            methodName,
            BindingFlags.Static | BindingFlags.NonPublic);

        Assert.NotNull(method);
        var parameter = Assert.Single(method!.GetParameters());
        Assert.Equal(typeof(ulong), parameter.ParameterType);
    }

    private static bool IsLowerSnakeFmf(string name) =>
        name.StartsWith("fmf_", StringComparison.Ordinal)
        && name.All(c => c is (>= 'a' and <= 'z') or (>= '0' and <= '9') or '_');
}
