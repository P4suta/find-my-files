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
    private IntPtr _handle;
    private GCHandle _self;

    public event Action<string>? IndexChanged;
    public event Action<VolumeStatus>? VolumeUpdated;

    public FfiEngineClient()
    {
        var indexDir = Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
            "find-my-files", "index");
        var config = $$"""{"index_dir": {{System.Text.Json.JsonSerializer.Serialize(indexDir)}}}""";
        var rc = NativeEngine.fmf_engine_create(config, out _handle);
        if (rc != NativeEngine.Ok)
        {
            NativeEngine.Throw(rc, "fmf_engine_create");
        }

        // The callback is an [UnmanagedCallersOnly] static — nothing the GC
        // can collect — and `user` carries a GCHandle back to this instance.
        _self = GCHandle.Alloc(this, GCHandleType.Weak);
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
        if (handle.Target is not FfiEngineClient self)
        {
            return;
        }
        string volume;
        int len = 0;
        while (len < 16 && ev->Volume[len] != 0)
        {
            len++;
        }
        volume = Encoding.UTF8.GetString(ev->Volume, len);

        switch (ev->Kind)
        {
            case 3: // IndexChanged
                self.IndexChanged?.Invoke(volume);
                break;
            case 1: // Progress
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Scanning, ev->Entries));
                break;
            case 2: // VolumeReady
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Ready, ev->Entries));
                break;
            case 4: // RescanStarted
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Rescanning, 0));
                break;
            case 5: // VolumeFailed
                self.VolumeUpdated?.Invoke(new VolumeStatus(volume, VolumeState.Failed, 0));
                break;
        }
    }

    public IReadOnlyList<string> ListVolumes()
    {
        var buf = stackalloc NativeEngine.FmfVolumeStatus[26];
        var rc = NativeEngine.fmf_list_volumes(_handle, buf, 26, out var count);
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
    }

    private static string LabelOf(in NativeEngine.FmfVolumeStatus s)
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

    public void StartIndexing(IReadOnlyList<string> volumes)
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
                var rc = NativeEngine.fmf_index_start(_handle, (byte**)pp, (uint)volumes.Count);
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
    }

    public IReadOnlyList<VolumeStatus> GetStatus()
    {
        var buf = stackalloc NativeEngine.FmfVolumeStatus[26];
        var rc = NativeEngine.fmf_index_status(_handle, buf, 26, out var count);
        if (rc != NativeEngine.Ok)
        {
            NativeEngine.Throw(rc, "fmf_index_status");
        }
        var result = new List<VolumeStatus>((int)count);
        for (var i = 0; i < count && i < 26; i++)
        {
            result.Add(new VolumeStatus(LabelOf(buf[i]), (VolumeState)buf[i].State, buf[i].Entries));
        }
        return result;
    }

    private static readonly System.Text.Json.JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = System.Text.Json.JsonNamingPolicy.SnakeCaseLower,
    };

    public Task<SearchOutcome> SearchAsync(string query, SearchOptions options)
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
                rc = NativeEngine.fmf_query(
                    handle, query, in native, out result, out count, out var trace);
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
                    .Deserialize<QueryTraceData>(traceJson, JsonOpts);
            }
            return new SearchOutcome(new FfiSearchResult(result, (long)count), traceData);
        });
    }

    public Task<EngineStatsData?> GetStatsAsync()
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
                : System.Text.Json.JsonSerializer.Deserialize<EngineStatsData>(json, JsonOpts);
        });
    }

    public void Dispose()
    {
        if (_handle != IntPtr.Zero)
        {
            NativeEngine.fmf_set_event_callback(_handle, null, IntPtr.Zero);
            NativeEngine.fmf_engine_destroy(_handle);
            _handle = IntPtr.Zero;
        }
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

    public Task<IReadOnlyList<RowData>> GetRangeAsync(long offset, int count)
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
                    var rows = new List<RowData>((int)page->RowCount);
                    var blob = new ReadOnlySpan<byte>(page->Blob.ToPointer(), (int)page->BlobLen);
                    var native = (NativeEngine.FmfRow*)page->Rows;
                    for (var i = 0; i < page->RowCount; i++)
                    {
                        ref readonly var r = ref native[i];
                        rows.Add(new RowData(
                            r.EntryRef,
                            r.Frn,
                            r.Size,
                            r.Mtime,
                            r.Flags,
                            Wtf8.Decode(blob.Slice((int)r.NameOff, r.NameLen)),
                            Wtf8.Decode(blob.Slice((int)r.ParentPathOff, r.ParentPathLen))));
                    }
                    return (IReadOnlyList<RowData>)rows;
                }
                finally
                {
                    NativeEngine.fmf_page_free(page);
                }
            }
            finally
            {
                if (added)
                {
                    DangerousRelease();
                }
            }
        });
    }
}
