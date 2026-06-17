using System.Runtime.InteropServices;

namespace FindMyFiles.Engine;

/// <summary>
/// Raw bindings to fmf_engine.dll. The DLL name is fixed (CLAUDE.md); the
/// struct layouts mirror docs/ARCHITECTURE.md exactly. No logic here.
/// </summary>
internal static partial class NativeEngine
{
    // The struct definitions live in Generated/EngineContract.g.cs (the
    // other half of this partial class — LayoutKind.Explicit with offsets
    // radiated from Rust offset_of!). These aliases keep the historical
    // spelling at the call sites; the values are the contract's.
    internal const int Ok = EngineContract.Status.Ok;
    internal const int Stale = EngineContract.Status.Stale;
    internal const int QuerySyntax = EngineContract.Status.QuerySyntax;
    internal const int Locked = EngineContract.Status.Locked;

    /// <summary>Marshaled sizes must equal the contract's — catches a stale
    /// Generated file at first touch, before any P/Invoke crosses.</summary>
    [System.Diagnostics.CodeAnalysis.SuppressMessage("Design", "CA1065:_", Justification = "deliberate fail-fast ABI tripwire: a TypeInitializationException at load is the intended failure when the marshaled layout drifts from the contract")]
    static NativeEngine()
    {
        if (Marshal.SizeOf<FmfRow>() != EngineContract.RowSize
            || Marshal.SizeOf<FmfEvent>() != EngineContract.EventSize
            || Marshal.SizeOf<FmfQueryOptions>() != EngineContract.QueryOptionsSize
            || Marshal.SizeOf<FmfVolumeStatus>() != EngineContract.VolumeStatusSize
            || Marshal.SizeOf<FmfPage>() != EngineContract.PageStructSize
            || Marshal.SizeOf<FmfBlob>() != EngineContract.BlobSize)
        {
            throw new InvalidOperationException(
                "EngineContract.g.cs layout disagrees with the marshaled structs — "
                + "regenerate with `just contract-gen` (ADR-0018)");
        }
    }

    // These P/Invoke entry points are named to match fmf_engine.dll's lowercase
    // `#[no_mangle]` C exports EXACTLY: LibraryImport uses the method name as the
    // symbol and GetProcAddress is case-sensitive, so a PascalCased name resolves
    // to nothing → EntryPointNotFoundException at the first call, silently demoting
    // in-proc / scope mode to the empty fake. SA1300 ("begin with uppercase") wants
    // PascalCase and would reintroduce exactly that break (it is the original cause:
    // it only flags entry points with no out/ref param, so the surface drifted half
    // PascalCased), so it is disabled across the binding block. NativeEngineBindingTests
    // pins the lowercase shape so any future drift fails the build instead of search.
#pragma warning disable SA1300 // FFI entry points mirror the DLL's lowercase C exports, not C# naming
    [LibraryImport("fmf_engine")]
    internal static partial uint fmf_abi_version();

    [LibraryImport("fmf_engine", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial int fmf_engine_create(string configJson, out IntPtr handle);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_engine_destroy(IntPtr handle);

    // Save-now for Ready, dirty volumes. The UI never calls this on its own;
    // it exists for in-proc parity with the service path (ARCHITECTURE.md).
    [LibraryImport("fmf_engine")]
    internal static partial int fmf_flush(IntPtr handle);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_set_event_callback(
        IntPtr handle,
        delegate* unmanaged[Cdecl]<FmfEvent*, IntPtr, void> cb,
        IntPtr user);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_list_volumes(
        IntPtr handle, FmfVolumeStatus* buf, uint cap, out uint count);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_index_start(IntPtr handle, byte** volumes, uint n);

    // ADR-0024: non-elevated scope-mode start over absolute root paths.
    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_index_start_scope(IntPtr handle, byte** roots, uint n);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_index_status(
        IntPtr handle, FmfVolumeStatus* buf, uint cap, out uint count);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_blob_free(FmfBlob* blob);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_engine_stats(IntPtr handle, out FmfBlob* blob);

    [LibraryImport("fmf_engine", StringMarshalling = StringMarshalling.Utf8)]
    internal static unsafe partial int fmf_query(
        IntPtr handle,
        string query,
        in FmfQueryOptions options,
        out IntPtr resultHandle,
        out ulong count,
        out FmfBlob* trace);

    internal static unsafe string? TakeBlob(FmfBlob* blob)
    {
        if (blob == null)
        {
            return null;
        }

        try
        {
            return System.Text.Encoding.UTF8.GetString((byte*)blob->Data, (int)blob->Len);
        }
        finally
        {
            _ = fmf_blob_free(blob);
        }
    }

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_result_page(
        IntPtr resultHandle, ulong offset, uint count, out FmfPage* page);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_page_free(FmfPage* page);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_result_free(IntPtr resultHandle);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_last_error(byte* buf, ref uint len);
#pragma warning restore SA1300

    internal static unsafe string LastError()
    {
        uint len = 1024;
        byte* buf = stackalloc byte[1024];
        _ = fmf_last_error(buf, ref len);
        return System.Text.Encoding.UTF8.GetString(buf, (int)len);
    }

    internal static void Throw(int code, string operation)
    {
        var detail = LastError();
        throw code switch
        {
            QuerySyntax => new QuerySyntaxException(detail),
            Stale => new StaleResultException(),
            _ => new EngineException($"{operation} failed ({code}): {detail}", code),
        };
    }
}
