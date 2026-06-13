using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;

namespace FindMyFiles.Engine;

/// <summary>
/// In-proc engine client over fmf_engine.dll. Events arrive on engine
/// threads; consumers marshal to the UI thread themselves.
/// </summary>
public sealed unsafe class FfiEngineClient : IEngineClient
{
    /// <summary>Global registration clock (advanced with Interlocked only).
    /// Every callback registration takes a unique generation from it, and
    /// Dispose advances the instance's live generation to another unique
    /// value, so "live == registered" can never hold again for that
    /// instance. This closes the dispose/recreate window: a native callback
    /// still in flight while Dispose runs resolves the instance but fails
    /// the generation check, instead of racing the engine teardown.</summary>
    private static long s_generation;

    private readonly long _registeredGeneration;
    private long _liveGeneration;

    private IntPtr _handle;
    private GCHandle _self;

    /// <inheritdoc/>
    public event Action<string>? IndexChanged;

    /// <inheritdoc/>
    public event Action<VolumeStatus>? VolumeUpdated;

    /// <inheritdoc/>
    public event Action<int>? EngineErrorOccurred;

    /// <summary>In-proc: no transport, no state transitions.</summary>
    public EngineConnectionState Connection => EngineConnectionState.InProc;

    /// <inheritdoc/>
    /// <remarks>In-proc has no transport, so this never fires; the
    /// add/remove accessors are empty.</remarks>
    public event Action<EngineConnectionState>? ConnectionChanged { add { } remove { } }

    /// <summary>Creates the in-proc engine over the default machine index at
    /// <c>%ProgramData%\find-my-files\index</c>.</summary>
    public FfiEngineClient()
        : this(Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
            "find-my-files", "index"))
    {
    }

    /// <summary>Test seam (contract suite): a throwaway index dir keeps the
    /// suite off %ProgramData% and out of the service's writer lock.</summary>
    internal FfiEngineClient(string indexDir)
    {
        var config = $$"""{"index_dir": {{System.Text.Json.JsonSerializer.Serialize(indexDir)}}}""";
        var rc = NativeEngine.fmf_engine_create(config, out _handle);
        if (rc != NativeEngine.Ok)
        {
            NativeEngine.Throw(rc, "fmf_engine_create");
        }

        // The callback is an [UnmanagedCallersOnly] static — nothing the GC
        // can collect — and `user` carries a GCHandle back to this instance.
        // The registration generation is recorded on the instance: events
        // only flow while the live generation still equals it (see OnEvent).
        _self = GCHandle.Alloc(this, GCHandleType.Weak);
        _registeredGeneration = Interlocked.Increment(ref s_generation);
        _liveGeneration = _registeredGeneration;
        rc = NativeEngine.fmf_set_event_callback(_handle, &OnEvent, GCHandle.ToIntPtr(_self));
        if (rc != NativeEngine.Ok)
        {
            NativeEngine.Throw(rc, "fmf_set_event_callback");
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static void OnEvent(NativeEngine.FmfEvent* ev, IntPtr user)
    {
        var handle = GCHandle.FromIntPtr(user);
        if (handle.Target is not FfiEngineClient self
            || Volatile.Read(ref self._liveGeneration) != self._registeredGeneration)
        {
            // Weak handle dead, handle slot recycled to another instance, or
            // the instance is mid-Dispose (generation already advanced) —
            // never deliver an event from a dying engine.
            return;
        }
        string volume;
        int len = 0;
        while (len < 16 && ev->Volume[len] != 0)
        {
            len++;
        }
        volume = Encoding.UTF8.GetString(ev->Volume, len);

        switch ((EventKind)ev->Kind)
        {
            case EventKind.IndexChanged:
                self.IndexChanged?.Invoke(volume);
                break;
            case EventKind.Progress:
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Scanning, ev->Entries));
                break;
            case EventKind.VolumeReady:
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Ready, ev->Entries));
                break;
            case EventKind.RescanStarted:
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Rescanning, 0));
                break;
            case EventKind.VolumeFailed:
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Failed, 0));
                break;
            case EventKind.EngineError: // Entries = severity 1..3
                self.EngineErrorOccurred?.Invoke((int)ev->Entries);
                break;
        }
    }

    // The three volume calls are cheap in-proc, but the interface contract
    // is async (the pipe client crosses a process boundary) — Task.Run keeps
    // the UI thread out of the FFI entirely. The ct goes to Task.Run: FFI
    // calls are short and non-cancellable mid-flight, so cancellation takes
    // effect at scheduling time (a pre-cancelled ct never crosses the FFI).
    /// <inheritdoc/>
    public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default)
    {
        var handle = _handle;
        return Task.Run<IReadOnlyList<string>>(() =>
        {
            var buf = stackalloc NativeEngine.FmfVolumeStatus[26];
            var rc = NativeEngine.fmf_list_volumes(handle, buf, 26, out var count);
            if (rc != NativeEngine.Ok)
            {
                NativeEngine.Throw(rc, "fmf_list_volumes");
            }
            var result = new List<string>((int)count);
            for (var i = 0; i < count && i < 26; i++)
            {
                result.Add(LabelOf(buf[i]));
            }
            return result;
        }, ct);
    }

#pragma warning disable RCS1242 // `in` is required to pin the fixed-size Label buffer via `fixed` below; FmfVolumeStatus is a generated marshaling struct, never mutated here
    private static string LabelOf(in NativeEngine.FmfVolumeStatus s)
#pragma warning restore RCS1242
    {
        fixed (byte* p = s.Label)
        {
            int len = 0;
            while (len < 16 && p[len] != 0)
            {
                len++;
            }
            return Encoding.UTF8.GetString(p, len);
        }
    }

    /// <inheritdoc/>
    public Task StartIndexingAsync(IReadOnlyList<string> volumes, CancellationToken ct = default)
    {
        var handle = _handle;
        return Task.Run(() =>
        {
            var ptrs = new IntPtr[volumes.Count];
            try
            {
                for (var i = 0; i < volumes.Count; i++)
                {
                    ptrs[i] = Marshal.StringToCoTaskMemUTF8(volumes[i]);
                }
                fixed (IntPtr* pp = ptrs)
                {
                    var rc = NativeEngine.fmf_index_start(handle, (byte**)pp, (uint)volumes.Count);
                    if (rc != NativeEngine.Ok)
                    {
                        NativeEngine.Throw(rc, "fmf_index_start");
                    }
                }
            }
            finally
            {
                foreach (var p in ptrs)
                {
                    if (p != IntPtr.Zero)
                    {
                        Marshal.FreeCoTaskMem(p);
                    }
                }
            }
        }, ct);
    }

    /// <inheritdoc/>
    public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default)
    {
        var handle = _handle;
        return Task.Run<IReadOnlyList<VolumeStatus>>(() =>
        {
            var buf = stackalloc NativeEngine.FmfVolumeStatus[26];
            var rc = NativeEngine.fmf_index_status(handle, buf, 26, out var count);
            if (rc != NativeEngine.Ok)
            {
                NativeEngine.Throw(rc, "fmf_index_status");
            }
            var result = new List<VolumeStatus>((int)count);
            for (var i = 0; i < count && i < 26; i++)
            {
                result.Add(new VolumeStatus(
                    LabelOf(buf[i]), (VolumeState)buf[i].State, buf[i].Entries));
            }
            return result;
        }, ct);
    }

    /// <inheritdoc/>
    public Task<SearchOutcome> SearchAsync(
        string query, SearchOptions options, CancellationToken ct = default)
    {
        var handle = _handle;
        return Task.Run(() =>
        {
            var native = new NativeEngine.FmfQueryOptions
            {
                Sort = (uint)options.Sort,
                Desc = options.Descending ? 1u : 0u,
                CaseMode = (uint)options.Case,
                IncludeHiddenSystem = options.IncludeHiddenSystem ? 1u : 0u,
            };
            int rc;
            IntPtr result;
            ulong count;
            string? traceJson;
            unsafe
            {
                // in matches the P/Invoke const-pointer ABI for FmfQueryOptions;
                // one-shot, never mutated.
#pragma warning disable RCS1242 // in matches the P/Invoke const-pointer ABI for FmfQueryOptions; one-shot, never mutated
                rc = NativeEngine.fmf_query(
                    handle, query, in native, out result, out count, out var trace);
#pragma warning restore RCS1242
                traceJson = rc == NativeEngine.Ok ? NativeEngine.TakeBlob(trace) : null;
            }
            if (rc != NativeEngine.Ok)
            {
                NativeEngine.Throw(rc, "fmf_query");
            }
            QueryTraceData? traceData = null;
            if (traceJson is not null)
            {
                traceData = System.Text.Json.JsonSerializer
                    .Deserialize<QueryTraceData>(traceJson, EngineJson.SnakeCase);
            }
            return new SearchOutcome(new FfiSearchResult(result, (long)count), traceData);
        }, ct);
    }

    /// <inheritdoc/>
    public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default)
    {
        var handle = _handle;
        return Task.Run(() =>
        {
            string? json;
            unsafe
            {
                var rc = NativeEngine.fmf_engine_stats(handle, out var blob);
                json = rc == NativeEngine.Ok ? NativeEngine.TakeBlob(blob) : null;
            }
            return json is null
                ? null
                : System.Text.Json.JsonSerializer
                    .Deserialize<EngineStatsData>(json, EngineJson.SnakeCase);
        }, ct);
    }

    /// <inheritdoc/>
    public void Dispose()
    {
        // Advance the generation FIRST: a native callback already in flight
        // on an engine thread resolves this instance but fails the
        // generation check, before fmf_set_event_callback(NULL) even lands.
        Volatile.Write(ref _liveGeneration, Interlocked.Increment(ref s_generation));
        if (_handle != IntPtr.Zero)
        {
            // Teardown return codes are intentionally ignored — there is no
            // recovery action during dispose.
            _ = NativeEngine.fmf_set_event_callback(_handle, null, IntPtr.Zero);
            _ = NativeEngine.fmf_engine_destroy(_handle); // joins engine threads
            _handle = IntPtr.Zero;
        }
        // Freed last: fmf_engine_destroy joined the threads that could still
        // dereference the handle, so no recycled-slot access is reachable.
        if (_self.IsAllocated)
        {
            _self.Free();
        }
    }
}

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
        return Task.Run<IReadOnlyList<RowData>>(() =>
        {
            // AddRef/Release keep the native result alive across an in-flight
            // fetch even if Dispose() races (docs/ARCHITECTURE.md C#契約).
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
        }, ct);
    }
}
