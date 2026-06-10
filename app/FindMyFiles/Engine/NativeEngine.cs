using System.Runtime.InteropServices;

namespace FindMyFiles.Engine;

/// <summary>
/// Raw bindings to fmf_engine.dll. The DLL name is fixed (CLAUDE.md); the
/// struct layouts mirror docs/ARCHITECTURE.md exactly. No logic here.
/// </summary>
internal static partial class NativeEngine
{
    internal const int Ok = 0;
    internal const int Stale = 2;
    internal const int QuerySyntax = 5;

    [StructLayout(LayoutKind.Sequential)]
    internal struct FmfQueryOptions
    {
        public uint Sort;
        public uint Desc;
        public uint CaseMode;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct FmfRow
    {
        public ulong EntryRef;
        public ulong Frn;
        public ulong Size;
        public long Mtime;
        public uint NameOff;
        public uint ParentPathOff;
        public uint Flags;
        public ushort NameLen;
        public ushort ParentPathLen;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct FmfPage
    {
        public uint RowCount;
        public uint _pad;
        public IntPtr Rows;
        public IntPtr Blob;
        public uint BlobLen;
        public uint _pad2;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal unsafe struct FmfEvent
    {
        public uint Kind;
        public uint _pad;
        public ulong Entries;
        public fixed byte Volume[16];
    }

    [StructLayout(LayoutKind.Sequential)]
    internal unsafe struct FmfVolumeStatus
    {
        public fixed byte Label[16];
        public uint State;
        public uint _pad;
        public ulong Entries;
    }

    [LibraryImport("fmf_engine")]
    internal static partial uint fmf_abi_version();

    [LibraryImport("fmf_engine", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial int fmf_engine_create(string configJson, out IntPtr handle);

    [LibraryImport("fmf_engine")]
    internal static partial int fmf_engine_destroy(IntPtr handle);

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

    [LibraryImport("fmf_engine", StringMarshalling = StringMarshalling.Utf8)]
    internal static partial int fmf_query(
        IntPtr handle,
        string query,
        in FmfQueryOptions options,
        out IntPtr resultHandle,
        out ulong count);

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
}
