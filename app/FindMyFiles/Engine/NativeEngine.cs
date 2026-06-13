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

/// <summary>
/// WTF-8 decoding: like UTF-8 but unpaired surrogates round-trip, matching
/// the engine's name pools. C# strings hold unpaired surrogates natively.
/// </summary>
internal static class Wtf8
{
    public static string Decode(ReadOnlySpan<byte> bytes)
    {
        var chars = new char[bytes.Length]; // UTF-16 units ≤ WTF-8 bytes
        int n = 0, i = 0;
        while (i < bytes.Length)
        {
            uint b0 = bytes[i];
            uint cp;
            int adv;
            if (b0 < 0x80) { cp = b0; adv = 1; }
            else if (b0 < 0xE0)
            {
                cp = ((b0 & 0x1F) << 6) | (uint)(bytes[i + 1] & 0x3F);
                adv = 2;
            }
            else if (b0 < 0xF0)
            {
                cp = ((b0 & 0x0F) << 12) | (uint)((bytes[i + 1] & 0x3F) << 6)
                     | (uint)(bytes[i + 2] & 0x3F);
                adv = 3;
            }
            else
            {
                cp = ((b0 & 0x07) << 18) | (uint)((bytes[i + 1] & 0x3F) << 12)
                     | (uint)((bytes[i + 2] & 0x3F) << 6) | (uint)(bytes[i + 3] & 0x3F);
                adv = 4;
            }
            i += adv;
            if (cp >= 0x10000)
            {
                cp -= 0x10000;
                chars[n++] = (char)(0xD800 + (cp >> 10));
                chars[n++] = (char)(0xDC00 + (cp & 0x3FF));
            }
            else
            {
                chars[n++] = (char)cp; // includes lone surrogates — intentional
            }
        }
        return new string(chars, 0, n);
    }

    /// <summary>WTF-8 encoding: the inverse of <see cref="Decode"/>. Lone
    /// surrogates become 3-byte sequences instead of U+FFFD.</summary>
    public static byte[] Encode(string s)
    {
        var bytes = new byte[s.Length * 3]; // WTF-8 bytes ≤ 3 × UTF-16 units
        int n = 0, i = 0;
        while (i < s.Length)
        {
            uint cp = s[i];
            if (char.IsHighSurrogate(s[i]) && i + 1 < s.Length && char.IsLowSurrogate(s[i + 1]))
            {
                cp = (uint)char.ConvertToUtf32(s[i], s[i + 1]);
                i += 2;
            }
            else
            {
                i++; // lone surrogates fall through as 3-byte sequences
            }
            if (cp < 0x80)
            {
                bytes[n++] = (byte)cp;
            }
            else if (cp < 0x800)
            {
                bytes[n++] = (byte)(0xC0 | (cp >> 6));
                bytes[n++] = (byte)(0x80 | (cp & 0x3F));
            }
            else if (cp < 0x10000)
            {
                bytes[n++] = (byte)(0xE0 | (cp >> 12));
                bytes[n++] = (byte)(0x80 | ((cp >> 6) & 0x3F));
                bytes[n++] = (byte)(0x80 | (cp & 0x3F));
            }
            else
            {
                bytes[n++] = (byte)(0xF0 | (cp >> 18));
                bytes[n++] = (byte)(0x80 | ((cp >> 12) & 0x3F));
                bytes[n++] = (byte)(0x80 | ((cp >> 6) & 0x3F));
                bytes[n++] = (byte)(0x80 | (cp & 0x3F));
            }
        }
        return bytes[..n];
    }
}
