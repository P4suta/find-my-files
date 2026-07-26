using System.Collections.Concurrent;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using FindMyFiles.Services;

namespace FindMyFiles.Engine;

/// <summary>
/// In-proc engine client over fmf_engine.dll. Every native operation holds a
/// SafeHandle reference for its complete call, so Dispose cannot free the Rust
/// engine while queued/in-flight Task.Run work still dereferences it. Events
/// arrive on engine threads and are exception-firewalled at the unmanaged
/// boundary; consumers marshal to the UI thread themselves.
/// </summary>
internal sealed unsafe class FfiEngineClient : IEngineClient
{
    private static long generation;

    private readonly long _registeredGeneration;
    private readonly FfiEngineSafeHandle _handle;
    private readonly ConcurrentDictionary<ulong, byte> _activeQueryControls = new();
    private long _liveGeneration;
    private int _disposed;

    /// <inheritdoc/>
    public EngineClientKind Kind => EngineClientKind.InProcess;

    /// <inheritdoc/>
    public event Action<string>? IndexChanged;

    /// <inheritdoc/>
    public event Action<VolumeStatus>? VolumeUpdated;

    /// <inheritdoc/>
    public event Action<int>? EngineErrorOccurred;

    /// <summary>In-proc: no transport, no state transitions.</summary>
    public EngineConnectionState Connection => EngineConnectionState.InProc;

    /// <inheritdoc/>
    public event Action<EngineConnectionState>? ConnectionChanged
    {
        add { }
        remove { }
    }

    /// <summary>Creates the in-proc engine over the default machine index at
    /// <c>%ProgramData%\find-my-files\index</c>.</summary>
    public FfiEngineClient()
        : this(Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
            "find-my-files",
            "index"))
    {
    }

    /// <summary>Test seam (contract suite): a throwaway index dir keeps the
    /// suite off %ProgramData% and out of the service's writer lock.</summary>
    /// <param name="indexDir">Directory holding the on-disk index.</param>
    /// <param name="logDir">When non-null, redirects the engine log here.</param>
    internal FfiEngineClient(string indexDir, string? logDir = null)
    {
        EnsureCompatibleAbi(NativeEngine.fmf_abi_version());

        var idx = System.Text.Json.JsonSerializer.Serialize(indexDir);
        var config = logDir is null
            ? $$"""{"index_dir": {{idx}}}"""
            : $$"""{"index_dir": {{idx}}, "log_dir": {{System.Text.Json.JsonSerializer.Serialize(logDir)}}}""";
        var rc = NativeEngine.fmf_engine_create(config, out var rawHandle);
        if (rc != NativeEngine.Ok)
        {
            NativeEngine.Throw(rc, "fmf_engine_create");
        }

        _registeredGeneration = Interlocked.Increment(ref generation);
        _liveGeneration = _registeredGeneration;
        var self = GCHandle.Alloc(this, GCHandleType.Weak);
        var user = GCHandle.ToIntPtr(self);
        try
        {
            rc = NativeEngine.fmf_set_event_callback(rawHandle, &OnEvent, user);
            if (rc != NativeEngine.Ok)
            {
                NativeEngine.Throw(rc, "fmf_set_event_callback");
            }

            _handle = new FfiEngineSafeHandle(rawHandle, user);
        }
        catch
        {
            // Registration failed before SafeHandle ownership transferred.
            // Tear down both native and managed handles on every exception path.
            _ = NativeEngine.fmf_set_event_callback(rawHandle, null, IntPtr.Zero);
            _ = NativeEngine.fmf_engine_destroy(rawHandle);
            if (self.IsAllocated)
            {
                self.Free();
            }

            throw;
        }
    }

    /// <summary>Reject an incompatible native DLL before any structured value
    /// or opaque handle crosses the ABI boundary.</summary>
    /// <param name="actual">ABI version reported by the loaded native DLL.</param>
    internal static void EnsureCompatibleAbi(uint actual)
    {
        if (actual != EngineContract.AbiVersion)
        {
            throw new EngineUnavailableException(
                $"fmf_engine ABI mismatch: app expects {EngineContract.AbiVersion}, "
                + $"loaded DLL reports {actual}");
        }
    }

    [UnmanagedCallersOnly(CallConvs = [typeof(CallConvCdecl)])]
    private static void OnEvent(NativeEngine.FmfEvent* ev, IntPtr user)
    {
        try
        {
            if (ev == null || user == IntPtr.Zero)
            {
                return;
            }

            var handle = GCHandle.FromIntPtr(user);
            if (handle.Target is not FfiEngineClient self
                || Volatile.Read(ref self._liveGeneration) != self._registeredGeneration)
            {
                return;
            }

            var len = 0;
            while (len < 16 && ev->Volume[len] != 0)
            {
                len++;
            }

            self.DispatchEvent(
                (EventKind)ev->Kind,
                Encoding.UTF8.GetString(ev->Volume, len),
                ev->Entries);
        }
        catch (Exception ex)
        {
            LogCallbackFailure(ex);
        }
    }

    private void DispatchEvent(EventKind kind, string volume, ulong entries)
    {
        try
        {
            switch (kind)
            {
                case EventKind.IndexChanged:
                    InvokeHandlers(IndexChanged, volume);
                    break;
                case EventKind.Progress:
                    InvokeHandlers(
                        VolumeUpdated,
                        new VolumeStatus(volume, VolumeState.Scanning, entries));
                    break;
                case EventKind.VolumeReady:
                    InvokeHandlers(
                        VolumeUpdated,
                        new VolumeStatus(volume, VolumeState.Ready, entries));
                    break;
                case EventKind.RescanStarted:
                    InvokeHandlers(
                        VolumeUpdated,
                        new VolumeStatus(volume, VolumeState.Rescanning, 0));
                    break;
                case EventKind.VolumeFailed:
                    InvokeHandlers(
                        VolumeUpdated,
                        new VolumeStatus(volume, VolumeState.Failed, 0));
                    break;
                case EventKind.EngineError:
                    InvokeHandlers(EngineErrorOccurred, (int)entries);
                    break;
                default:
                    FileLog.WarnEvent(
                        "ffi",
                        "unknown engine event",
                        fields: [("event_kind", (int)kind)]);
                    break;
            }
        }
        catch (Exception ex)
        {
            LogCallbackFailure(ex);
        }
    }

    private static void InvokeHandlers<T>(Action<T>? handlers, T value)
    {
        if (handlers is null)
        {
            return;
        }

        foreach (Action<T> handler in handlers.GetInvocationList())
        {
            try
            {
                handler(value);
            }
            catch (Exception ex)
            {
                LogCallbackFailure(ex);
            }
        }
    }

#if FMF_TEST_SEAMS
    /// <summary>Exercises the unmanaged callback's managed dispatch firewall
    /// without requiring a live native worker thread.</summary>
    /// <param name="kind">Synthetic event kind.</param>
    /// <param name="volume">Synthetic event volume label.</param>
    /// <param name="entries">Synthetic event entry count or payload.</param>
    internal void DispatchEventForTests(EventKind kind, string volume, ulong entries) =>
        DispatchEvent(kind, volume, entries);
#endif

    private static void LogCallbackFailure(Exception ex)
    {
        try
        {
            FileLog.Error("ffi", "engine event handler failed", ex);
        }
        catch
        {
            // Nothing may escape an UnmanagedCallersOnly boundary, including
            // a secondary diagnostics failure.
        }
    }

    private T WithHandle<T>(Func<IntPtr, T> call)
    {
        var added = false;
        _handle.DangerousAddRef(ref added);
        try
        {
            return call(_handle.DangerousGetHandle());
        }
        finally
        {
            if (added)
            {
                _handle.DangerousRelease();
            }
        }
    }

    private void WithHandle(Action<IntPtr> call) =>
        WithHandle(
            handle =>
            {
                call(handle);
                return true;
            });

    /// <inheritdoc/>
    public Task<IReadOnlyList<string>> ListVolumesAsync(CancellationToken ct = default) =>
        Task.Run<IReadOnlyList<string>>(
            () => WithHandle(handle =>
            {
                const uint capacity = EngineContract.MaxVolumes;
                var buf = stackalloc NativeEngine.FmfVolumeStatus[EngineContract.MaxVolumes];
                var rc = NativeEngine.fmf_list_volumes(handle, buf, capacity, out var count);
                if (rc != NativeEngine.Ok)
                {
                    NativeEngine.Throw(rc, "fmf_list_volumes");
                }

                var validCount = NativeEngine.ValidateVolumeCount(count, capacity);
                var result = new List<string>(validCount);
                for (var i = 0; i < validCount; i++)
                {
                    result.Add(LabelOf(buf[i]));
                }

                return (IReadOnlyList<string>)result;
            }),
            ct);

#pragma warning disable RCS1242
    private static string LabelOf(in NativeEngine.FmfVolumeStatus status)
#pragma warning restore RCS1242
    {
        fixed (byte* p = status.Label)
        {
            var len = 0;
            while (len < 16 && p[len] != 0)
            {
                len++;
            }

            return Encoding.UTF8.GetString(p, len);
        }
    }

    private static IntPtr[] MarshalUtf8(string[] items)
    {
        var ptrs = new IntPtr[items.Length];
        try
        {
            for (var i = 0; i < items.Length; i++)
            {
                ptrs[i] = Marshal.StringToCoTaskMemUTF8(items[i]);
            }

            return ptrs;
        }
        catch
        {
            FreeUtf8(ptrs);
            throw;
        }
    }

    private static void FreeUtf8(IntPtr[] ptrs)
    {
        foreach (var ptr in ptrs)
        {
            if (ptr != IntPtr.Zero)
            {
                Marshal.FreeCoTaskMem(ptr);
            }
        }
    }

    /// <inheritdoc/>
    public Task StartIndexingAsync(
        IReadOnlyList<string> volumes,
        CancellationToken ct = default)
    {
        var snapshot = EngineRequest.Volumes(volumes);
        return Task.Run(
            () => WithHandle(handle =>
            {
                var volumePtrs = MarshalUtf8(snapshot);
                try
                {
                    fixed (IntPtr* vp = volumePtrs)
                    {
                        var rc = NativeEngine.fmf_index_start(
                            handle,
                            (byte**)vp,
                            (uint)snapshot.Length);
                        if (rc != NativeEngine.Ok)
                        {
                            NativeEngine.Throw(rc, "fmf_index_start");
                        }
                    }
                }
                finally
                {
                    FreeUtf8(volumePtrs);
                }
            }),
            ct);
    }

    /// <inheritdoc/>
    public Task<IReadOnlyList<VolumeStatus>> GetStatusAsync(CancellationToken ct = default) =>
        Task.Run<IReadOnlyList<VolumeStatus>>(
            () => WithHandle(handle =>
            {
                const uint capacity = EngineContract.MaxVolumes;
                var buf = stackalloc NativeEngine.FmfVolumeStatus[EngineContract.MaxVolumes];
                var rc = NativeEngine.fmf_index_status(handle, buf, capacity, out var count);
                if (rc != NativeEngine.Ok)
                {
                    NativeEngine.Throw(rc, "fmf_index_status");
                }

                var validCount = NativeEngine.ValidateVolumeCount(count, capacity);
                var result = new List<VolumeStatus>(validCount);
                for (var i = 0; i < validCount; i++)
                {
                    result.Add(new VolumeStatus(
                        LabelOf(buf[i]),
                        (VolumeState)buf[i].State,
                        buf[i].Entries));
                }

                return (IReadOnlyList<VolumeStatus>)result;
            }),
            ct);

    /// <inheritdoc/>
    public Task<SearchOutcome> SearchAsync(
        string query,
        SearchOptions options,
        CancellationToken ct = default) =>
        SearchAsync(query, options, null, ct);

    /// <inheritdoc/>
    public Task<SearchOutcome> SearchAsync(
        string query,
        SearchOptions options,
        ISearchResult? presentationBasis,
        CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        var checkedQuery = EngineRequest.QueryText(query);
        if (presentationBasis is not null and not FfiSearchResult)
        {
            throw new ArgumentException(
                "presentation basis belongs to a different engine transport",
                nameof(presentationBasis));
        }

        var engineAdded = false;
        var basisAdded = false;
        ulong controlId = 0;
        CancellationTokenRegistration cancellationRegistration = default;
        _handle.DangerousAddRef(ref engineAdded);
        try
        {
            var handle = _handle.DangerousGetHandle();
            var rc = NativeEngine.fmf_query_control_create(handle, out controlId);
            if (rc != NativeEngine.Ok)
            {
                NativeEngine.Throw(rc, "fmf_query_control_create");
            }

            _activeQueryControls.TryAdd(controlId, 0);
            if (Volatile.Read(ref _disposed) != 0)
            {
                _ = NativeEngine.fmf_query_control_cancel(controlId);
                throw new ObjectDisposedException(nameof(FfiEngineClient));
            }

            cancellationRegistration = ct.UnsafeRegister(
                static state => CancelQueryControl((ulong)state!),
                controlId);

            ulong basisId = 0;
            if (presentationBasis is FfiSearchResult ffiBasis)
            {
                try
                {
                    ffiBasis.DangerousAddRef(ref basisAdded);
                    basisId = unchecked((ulong)ffiBasis.DangerousGetHandle().ToInt64());
                }
                catch (ObjectDisposedException)
                {
                    basisAdded = false;
                }
            }

            var queryTask = Task.Run(
                () =>
            {
                var native = new NativeEngine.FmfQueryOptions
                {
                    Sort = (uint)options.Sort,
                    Desc = options.Descending ? 1u : 0u,
                    CaseMode = (uint)options.Case,
                    IncludeHiddenSystem = options.IncludeHiddenSystem ? 1u : 0u,
                    RegexMode = options.RegexModeBits,
                    Reserved = 0,
                    PresentationBasis = basisId,
                };
                IntPtr result = IntPtr.Zero;
                ulong count = 0;
                NativeEngine.FmfBlob* trace = null;
                ulong traceOwnerId = 0;
                FfiSearchResult? owned = null;

                try
                {
#pragma warning disable RCS1242
                    var queryRc = NativeEngine.fmf_query(
                        handle,
                        checkedQuery,
                        in native,
                        controlId,
                        out result,
                        out count,
                        out trace);
#pragma warning restore RCS1242
                    if (trace != null)
                    {
                        traceOwnerId = trace->OwnerId;
                    }

                    if (queryRc == NativeEngine.Cancelled)
                    {
                        throw new OperationCanceledException("query cancelled", ct);
                    }

                    if (queryRc != NativeEngine.Ok)
                    {
                        NativeEngine.Throw(queryRc, "fmf_query");
                    }

                    var resultId = unchecked((ulong)result.ToInt64());
                    owned = FfiSearchResult.TakeOwnership(ref result, count);
                    var ownedTrace = trace;
                    trace = null;
                    traceOwnerId = 0;
                    var traceJson = NativeEngine.TakeBlob(ownedTrace);
                    QueryTraceData? traceData = traceJson is null
                        ? null
                        : System.Text.Json.JsonSerializer.Deserialize<QueryTraceData>(
                            traceJson,
                            EngineJson.SnakeCase);
                    if (traceData is null || !traceData.Unchanged)
                    {
                        FileLog.Event(
                            "query",
                            "query served",
                            ("rid", resultId),
                            ("hits", count));
                    }

                    var outcome = new SearchOutcome(owned, traceData);
                    owned = null;
                    return outcome;
                }
                finally
                {
                    owned?.Dispose();
                    if (result != IntPtr.Zero)
                    {
                        _ = NativeEngine.fmf_result_free(result);
                    }

                    _ = NativeEngine.fmf_blob_free(traceOwnerId);
                }
            },
                CancellationToken.None);
            return queryTask.ContinueWith(
                completed =>
                {
                    try
                    {
                        return completed.GetAwaiter().GetResult();
                    }
                    finally
                    {
                        Cleanup();
                    }
                },
                CancellationToken.None,
                TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default);
        }
        catch
        {
            Cleanup();
            throw;
        }

        void Cleanup()
        {
            // Dispose waits for an in-flight callback. Only after it is
            // impossible to issue another cancel do we remove/free the
            // native control; the SafeHandle admission remains held through
            // the entire lifecycle.
            cancellationRegistration.Dispose();
            if (controlId != 0)
            {
                _activeQueryControls.TryRemove(controlId, out _);
                _ = NativeEngine.fmf_query_control_free(controlId);
            }

            if (basisAdded && presentationBasis is FfiSearchResult ffiBasis)
            {
                ffiBasis.DangerousRelease();
            }

            if (engineAdded)
            {
                _handle.DangerousRelease();
            }
        }
    }

    private static void CancelQueryControl(ulong controlId)
    {
        try
        {
            _ = NativeEngine.fmf_query_control_cancel(controlId);
        }
        catch (Exception ex)
        {
            LogCallbackFailure(ex);
        }
    }

    /// <inheritdoc/>
    public Task<EngineStatsData?> GetStatsAsync(CancellationToken ct = default) =>
        Task.Run(
            () => WithHandle<EngineStatsData?>(handle =>
            {
                NativeEngine.FmfBlob* blob = null;
                ulong blobOwnerId = 0;
                try
                {
                    var rc = NativeEngine.fmf_engine_stats(handle, out blob);
                    if (blob != null)
                    {
                        blobOwnerId = blob->OwnerId;
                    }

                    if (rc != NativeEngine.Ok)
                    {
                        NativeEngine.Throw(rc, "fmf_engine_stats");
                    }

                    var ownedBlob = blob;
                    blob = null;
                    blobOwnerId = 0;
                    var json = NativeEngine.TakeBlob(ownedBlob);
                    if (json is null)
                    {
                        throw new InvalidDataException(
                            "The native stats request succeeded without returning a blob.");
                    }

                    return System.Text.Json.JsonSerializer.Deserialize<EngineStatsData>(
                        json,
                        EngineJson.SnakeCase);
                }
                finally
                {
                    _ = NativeEngine.fmf_blob_free(blobOwnerId);
                }
            }),
            ct);

    /// <inheritdoc/>
    public void Dispose()
    {
        if (Interlocked.Exchange(ref _disposed, 1) != 0)
        {
            return;
        }

        // Suppress callbacks first. SafeHandle.Dispose then rejects new
        // DangerousAddRef calls and defers ReleaseHandle until every operation
        // that already acquired a reference has returned.
        Volatile.Write(ref _liveGeneration, Interlocked.Increment(ref generation));
        foreach (var controlId in _activeQueryControls.Keys)
        {
            CancelQueryControl(controlId);
        }

        _handle.Dispose();
    }

    /// <summary>
    /// Owns the Rust engine pointer and the callback GCHandle as one lifetime.
    /// ReleaseHandle runs only after all operation references drain; native
    /// destroy joins worker threads before the callback user handle is freed.
    /// </summary>
    private sealed class FfiEngineSafeHandle : SafeHandle
    {
        private readonly IntPtr _callbackUser;

        internal FfiEngineSafeHandle(IntPtr handle, IntPtr callbackUser)
            : base(IntPtr.Zero, ownsHandle: true)
        {
            SetHandle(handle);
            _callbackUser = callbackUser;
        }

        public override bool IsInvalid => handle == IntPtr.Zero;

        protected override bool ReleaseHandle()
        {
            var destroyed = false;
            var callbackDetached = false;
            try
            {
                callbackDetached =
                    NativeEngine.fmf_set_event_callback(handle, null, IntPtr.Zero)
                    == NativeEngine.Ok;
                destroyed = NativeEngine.fmf_engine_destroy(handle) == NativeEngine.Ok;
                callbackDetached |= destroyed;
            }
            catch (Exception ex)
            {
                LogCallbackFailure(ex);
            }
            finally
            {
                try
                {
                    // If native teardown failed before either unregister or
                    // destroy crossed its quiescence barrier, leaking the weak
                    // GCHandle is safer than letting a late callback observe a
                    // recycled managed handle.
                    if (callbackDetached && _callbackUser != IntPtr.Zero)
                    {
                        var self = GCHandle.FromIntPtr(_callbackUser);
                        if (self.IsAllocated)
                        {
                            self.Free();
                        }
                    }
                }
                catch (Exception ex)
                {
                    LogCallbackFailure(ex);
                }
            }

            return destroyed;
        }
    }
}
