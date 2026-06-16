//! Event kinds — FFI callback `FmfEvent.kind` and pipe event-push opcodes
//! carry the same values (docs/ARCHITECTURE.md イベント節).

/// `u32` on the wire and in the FFI POD.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// `entries` = scanned count so far.
    Progress = 1,
    /// `entries` = total entries of the now-Ready volume.
    VolumeReady = 2,
    /// Engine-side 200ms debounce — the only throttle in the pipeline.
    IndexChanged = 3,
    /// A full rescan of a volume has begun (snapshot/journal recovery).
    RescanStarted = 4,
    /// The volume could not be indexed and is no longer being served.
    VolumeFailed = 5,
    /// `entries` = severity ([`SEVERITY_WARN`] etc.); details are pulled
    /// from the stats JSON (push notification + pull detail).
    EngineError = 6,
}

/// Wire/FFI value of [`EventKind::Progress`].
pub const FMF_EVENT_PROGRESS: u32 = EventKind::Progress as u32;
/// Wire/FFI value of [`EventKind::VolumeReady`].
pub const FMF_EVENT_VOLUME_READY: u32 = EventKind::VolumeReady as u32;
/// Wire/FFI value of [`EventKind::IndexChanged`].
pub const FMF_EVENT_INDEX_CHANGED: u32 = EventKind::IndexChanged as u32;
/// Wire/FFI value of [`EventKind::RescanStarted`].
pub const FMF_EVENT_RESCAN_STARTED: u32 = EventKind::RescanStarted as u32;
/// Wire/FFI value of [`EventKind::VolumeFailed`].
pub const FMF_EVENT_VOLUME_FAILED: u32 = EventKind::VolumeFailed as u32;
/// Wire/FFI value of [`EventKind::EngineError`].
pub const FMF_EVENT_ENGINE_ERROR: u32 = EventKind::EngineError as u32;

/// Severity values carried in `FmfEvent.entries` for [`EventKind::EngineError`].
pub const SEVERITY_WARN: u64 = 1;
/// A recoverable error degraded a path but the engine keeps serving.
pub const SEVERITY_ERROR: u64 = 2;
/// A caught panic crossed the FFI boundary (recovered via `catch_unwind`).
pub const SEVERITY_PANIC: u64 = 3;
