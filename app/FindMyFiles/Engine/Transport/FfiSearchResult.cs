using System.Runtime.InteropServices;

namespace FindMyFiles.Engine;

/// <summary>
/// A native query result held as a <see cref="SafeHandle"/>. Every page fetch
/// brackets itself with DangerousAddRef/DangerousRelease, so a
/// <c>Dispose()</c> racing an in-flight fetch defers
/// <c>fmf_result_free</c> until that fetch has finished reading the native
/// result — the handle can never be freed under a live page read.
/// </summary>
internal sealed unsafe class FfiSearchResult : SafeHandle, ISearchResult
{
    private readonly object _owner;

    private FfiSearchResult(IntPtr handle, object owner, bool ownsHandle = true)
        : base(IntPtr.Zero, ownsHandle)
    {
        _owner = owner;
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
    /// <param name="owner">Identity of the FFI engine session that created it.</param>
    /// <returns>The owning managed result.</returns>
    internal static FfiSearchResult TakeOwnership(
        ref IntPtr handle,
        ulong count,
        object owner)
    {
        if (handle == IntPtr.Zero)
        {
            throw new InvalidDataException(
                "The native query succeeded without returning a result handle.");
        }

        var owned = new FfiSearchResult(handle, owner);
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

    /// <summary>
    /// Acquires a SafeHandle reference only when this result belongs to the
    /// exact engine session issuing the next query. Foreign or disposed
    /// results behave as no presentation basis.
    /// </summary>
    /// <param name="expectedOwner">Identity of the engine session issuing the query.</param>
    /// <param name="added">Set when the caller must later call DangerousRelease.</param>
    /// <param name="id">The owned native result ID, or zero when unavailable.</param>
    /// <returns>True only when a live same-session basis was acquired.</returns>
    internal bool TryAcquirePresentationBasis(
        object expectedOwner,
        out bool added,
        out ulong id)
    {
        added = false;
        id = 0;
        if (!ReferenceEquals(_owner, expectedOwner))
        {
            return false;
        }

        try
        {
            DangerousAddRef(ref added);
            var raw = DangerousGetHandle();
            if (raw == IntPtr.Zero)
            {
                if (added)
                {
                    DangerousRelease();
                    added = false;
                }

                return false;
            }

            id = unchecked((ulong)raw.ToInt64());
            return true;
        }
        catch (ObjectDisposedException)
        {
            added = false;
            return false;
        }
    }

#if FMF_TEST_SEAMS
    internal static FfiSearchResult CreateNonOwningForTests(
        IntPtr handle,
        object owner) =>
        new(handle, owner, ownsHandle: false);
#endif

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
            // fetch even if Dispose() races (see the class summary).
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
