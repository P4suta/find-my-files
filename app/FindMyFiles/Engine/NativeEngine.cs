using System.Runtime.InteropServices;

namespace FindMyFiles.Engine;

/// <summary>
/// Raw bindings and ABI-boundary validation for fmf_engine.dll. The DLL name
/// is fixed (AGENTS.md); every struct layout, status code and limit used here
/// is radiated from the fmf-contract crate into
/// <see cref="EngineContract"/> (ADR-0018), never hand-written. Engine
/// behavior remains outside this interop boundary.
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
    internal const int Cancelled = EngineContract.Status.Cancelled;

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
    // to nothing → EntryPointNotFoundException at the first call. SA1300
    // ("begin with uppercase") wants
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

    // Save-now for Ready, dirty volumes. The UI never calls this on its own:
    // the pipe deliberately exposes no flush opcode (a client-driven flush is a
    // local DoS on the index read lock — ADR-0016), so saving stays a
    // service-internal schedule. This export exists for in-proc parity only.
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

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_index_status(
        IntPtr handle, FmfVolumeStatus* buf, uint cap, out uint count);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_blob_free(ulong ownerId);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_engine_stats(IntPtr handle, out FmfBlob* blob);

    [LibraryImport("fmf_engine", StringMarshalling = StringMarshalling.Utf8)]
    internal static unsafe partial int fmf_query(
        IntPtr handle,
        string query,
        in FmfQueryOptions options,
        ulong queryControlId,
        out IntPtr resultHandle,
        out ulong count,
        out FmfBlob* trace);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_query_control_create(
        IntPtr handle,
        out ulong queryControlId);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_query_control_cancel(ulong queryControlId);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_query_control_free(ulong queryControlId);

    internal static unsafe string? TakeBlob(FmfBlob* blob)
    {
        if (blob == null)
        {
            return null;
        }

        var descriptor = *blob;
        var ownerId = descriptor.OwnerId;
        try
        {
            var length = ValidateBlob(descriptor);
            return length == 0
                ? string.Empty
                : System.Text.Encoding.UTF8.GetString(
                    (byte*)descriptor.Data,
                    length);
        }
        finally
        {
            _ = fmf_blob_free(ownerId);
        }
    }

    /// <summary>
    /// Validates an engine-owned blob before a managed span/string observes
    /// its pointer. The pointer is present exactly when the byte length is
    /// positive, and the allocation cannot exceed the shared payload cap.
    /// </summary>
    /// <param name="blob">Native blob descriptor to validate.</param>
    /// <returns>The checked managed byte length.</returns>
    internal static int ValidateBlob(FmfBlob blob)
    {
        if (blob.OwnerId == 0)
        {
            throw new InvalidDataException(
                "Native blob descriptor has no allocation owner ID.");
        }

        if ((blob.Data == IntPtr.Zero) != (blob.Len == 0))
        {
            throw new InvalidDataException(
                "Native blob pointer and length disagree.");
        }

        if (blob.Len > EngineContract.MaxPayloadLen)
        {
            throw new InvalidDataException(
                $"Native blob is {blob.Len} bytes; maximum is "
                + $"{EngineContract.MaxPayloadLen}.");
        }

        return checked((int)blob.Len);
    }

    /// <summary>
    /// Validates a native result page before either native buffer is exposed
    /// as a managed span.
    /// </summary>
    /// <param name="page">Native page descriptor to validate.</param>
    /// <param name="requestedCount">Row count requested from the engine.</param>
    /// <returns>Checked row-buffer and blob lengths.</returns>
    internal static (int RowBytes, int BlobBytes) ValidatePage(
        FmfPage page,
        uint requestedCount)
    {
        if (page.OwnerId == 0)
        {
            throw new InvalidDataException(
                "Native page descriptor has no allocation owner ID.");
        }

        if (requestedCount > (uint)EngineContract.MaxPageRows)
        {
            throw new InvalidDataException(
                $"Native page request count {requestedCount} exceeds the "
                + $"{EngineContract.MaxPageRows}-row cap.");
        }

        if (page.RowCount > requestedCount
            || page.RowCount > (uint)EngineContract.MaxPageRows)
        {
            throw new InvalidDataException(
                $"Native page returned {page.RowCount} rows for a "
                + $"{requestedCount}-row request.");
        }

        if ((page.Rows == IntPtr.Zero) != (page.RowCount == 0))
        {
            throw new InvalidDataException(
                "Native page row pointer and count disagree.");
        }

        if ((page.Blob == IntPtr.Zero) != (page.BlobLen == 0))
        {
            throw new InvalidDataException(
                "Native page blob pointer and length disagree.");
        }

        if (page.BlobLen > EngineContract.MaxPayloadLen)
        {
            throw new InvalidDataException(
                $"Native page blob is {page.BlobLen} bytes; maximum is "
                + $"{EngineContract.MaxPayloadLen}.");
        }

        var rowBytes = checked((ulong)page.RowCount * (ulong)EngineContract.RowSize);
        var payloadBytes = checked(8UL + rowBytes + page.BlobLen);
        if (payloadBytes > EngineContract.MaxPayloadLen)
        {
            throw new InvalidDataException(
                $"Native page payload is {payloadBytes} bytes; maximum is "
                + $"{EngineContract.MaxPayloadLen}.");
        }

        return (checked((int)rowBytes), checked((int)page.BlobLen));
    }

    /// <summary>Validates a native volume-array count before allocation.</summary>
    /// <param name="count">Element count returned by the engine.</param>
    /// <param name="capacity">Capacity supplied to the engine.</param>
    /// <returns>The checked managed element count.</returns>
    internal static int ValidateVolumeCount(uint count, uint capacity)
    {
        if (capacity > (uint)EngineContract.MaxVolumes)
        {
            throw new ArgumentOutOfRangeException(
                nameof(capacity),
                capacity,
                $"Volume buffer capacity cannot exceed {EngineContract.MaxVolumes}.");
        }

        if (count > capacity || count > (uint)EngineContract.MaxVolumes)
        {
            throw new InvalidDataException(
                $"Native volume count {count} exceeds the supplied "
                + $"{capacity}-element buffer.");
        }

        return checked((int)count);
    }

    /// <summary>Converts an ABI result count without signed wraparound.</summary>
    /// <param name="count">Unsigned result count returned by the engine.</param>
    /// <returns>The checked managed result count.</returns>
    internal static long ValidateResultCount(ulong count)
    {
        if (count > long.MaxValue)
        {
            throw new InvalidDataException(
                $"Native result count {count} exceeds the managed range.");
        }

        return checked((long)count);
    }

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_result_page(
        IntPtr resultHandle, ulong offset, uint count, out FmfPage* page);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_page_free(ulong ownerId);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_result_free(IntPtr resultHandle);

    [LibraryImport("fmf_engine")]
    internal static unsafe partial int fmf_last_error(byte* buf, ref uint len);
#pragma warning restore SA1300

    internal static unsafe string LastError()
    {
        uint required = 0;
        if (fmf_last_error(null, ref required) != Ok || required == 0)
        {
            return string.Empty;
        }

        var bytes = new byte[checked((int)required + 1)];
        fixed (byte* buf = bytes)
        {
            var capacity = (uint)bytes.Length;
            if (fmf_last_error(buf, ref capacity) != Ok)
            {
                return string.Empty;
            }

            return System.Text.Encoding.UTF8.GetString(bytes, 0, checked((int)capacity));
        }
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
