using System.Runtime.InteropServices;

namespace FindMyFiles.Engine;

internal sealed unsafe class FfiSearchResult(IntPtr handle, long count) : SafeHandle(handle, true), ISearchResult
{
    public long Count { get; } = count;

    public override bool IsInvalid => this.handle == IntPtr.Zero;

    protected override bool ReleaseHandle()
    {
        return NativeEngine.fmf_result_free(this.handle) == NativeEngine.Ok;
    }

    public Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default)
    {
        return Task.Run<IReadOnlyList<RowData>>(
            () =>
        {
            // AddRef/Release keep the native result alive across an in-flight
            // fetch even if Dispose() races (docs/ARCHITECTURE.md C# contract).
            var added = false;
            DangerousAddRef(ref added);
            try
            {
                var rc = NativeEngine.fmf_result_page(
                    handle, (ulong)offset, (uint)count, out var page);
                if (rc != NativeEngine.Ok)
                {
                    NativeEngine.Throw(rc, "fmf_result_page");
                }

                try
                {
                    // The native page is the same layout the pipe carries:
                    // 48-byte rows + blob, decoded by the shared PageCodec.
                    return (IReadOnlyList<RowData>)PageCodec.Decode(
                        new ReadOnlySpan<byte>(
                            page->Rows.ToPointer(), (int)page->RowCount * PageCodec.RowSize),
                        new ReadOnlySpan<byte>(page->Blob.ToPointer(), (int)page->BlobLen));
                }
                finally
                {
                    // Free path: the return code carries no recovery action.
                    _ = NativeEngine.fmf_page_free(page);
                }
            }
            finally
            {
                if (added)
                {
                    DangerousRelease();
                }
            }
        },
            ct);
    }
}
