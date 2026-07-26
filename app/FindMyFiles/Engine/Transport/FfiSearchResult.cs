using System.Runtime.InteropServices;

namespace FindMyFiles.Engine;

internal sealed unsafe class FfiSearchResult : SafeHandle, ISearchResult
{
    private FfiSearchResult(IntPtr handle)
        : base(IntPtr.Zero, ownsHandle: true)
    {
        SetHandle(handle);
    }

    public long Count { get; private set; }

    public override bool IsInvalid => this.handle == IntPtr.Zero;

    /// <summary>
    /// Takes ownership of a successful query's native handle before validating
    /// any other returned field. A malformed count therefore cannot orphan the
    /// native result allocation.
    /// </summary>
    /// <param name="handle">
    /// Native result handle, cleared when ownership transfers.
    /// </param>
    /// <param name="count">Unsigned result count returned beside the handle.</param>
    /// <returns>The owning managed result.</returns>
    internal static FfiSearchResult TakeOwnership(ref IntPtr handle, ulong count)
    {
        if (handle == IntPtr.Zero)
        {
            throw new InvalidDataException(
                "The native query succeeded without returning a result handle.");
        }

        var owned = new FfiSearchResult(handle);
        handle = IntPtr.Zero;
        try
        {
            owned.Count = NativeEngine.ValidateResultCount(count);
            return owned;
        }
        catch
        {
            owned.Dispose();
            throw;
        }
    }

    protected override bool ReleaseHandle()
    {
        return NativeEngine.fmf_result_free(this.handle) == NativeEngine.Ok;
    }

    public Task<IReadOnlyList<RowData>> GetRangeAsync(
        long offset, int count, CancellationToken ct = default)
    {
        var request = EngineRequest.PageRange(offset, count);
        return Task.Run<IReadOnlyList<RowData>>(
            () =>
        {
            // AddRef/Release keep the native result alive across an in-flight
            // fetch even if Dispose() races (docs/ARCHITECTURE.md C# contract).
            var added = false;
            DangerousAddRef(ref added);
            try
            {
                NativeEngine.FmfPage* page = null;
                NativeEngine.FmfPage descriptor = default;
                ulong ownerId = 0;
                try
                {
                    var rc = NativeEngine.fmf_result_page(
                        handle, request.Offset, request.Count, out page);
                    if (page != null)
                    {
                        descriptor = *page;
                        ownerId = descriptor.OwnerId;
                    }

                    if (rc != NativeEngine.Ok)
                    {
                        NativeEngine.Throw(rc, "fmf_result_page");
                    }

                    if (page == null)
                    {
                        throw new InvalidDataException(
                            "The native page request succeeded without returning a page.");
                    }

                    var lengths = NativeEngine.ValidatePage(descriptor, request.Count);

                    // The native page is the same layout the pipe carries:
                    // 56-byte rows + blob, decoded by the shared PageCodec.
                    return (IReadOnlyList<RowData>)PageCodec.Decode(
                        new ReadOnlySpan<byte>(
                            descriptor.Rows.ToPointer(), lengths.RowBytes),
                        new ReadOnlySpan<byte>(
                            descriptor.Blob.ToPointer(),
                            lengths.BlobBytes));
                }
                finally
                {
                    // Free every allocation the native side returned, including
                    // malformed success values and unexpected error outputs.
                    _ = NativeEngine.fmf_page_free(ownerId);
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
