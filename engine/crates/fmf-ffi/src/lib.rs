//! fmf-ffi — the in-process C ABI surface over fmf-core, built as the
//! `fmf_engine` cdylib. Conversion, handle management and panic catching
//! only: the logic lives in fmf-core, and every entry point here maps 1:1
//! onto a named-pipe opcode in [`fmf_contract::opcodes`].
//!
//! The values crossing this boundary (status codes, event kinds, POD layout,
//! limits, ABI version) come from `fmf-contract` (ADR-0018); this crate's
//! `contract_tests` re-assert them against literals as an independent
//! tripwire against a miss-edit of that crate.
//!
//! # Boundary conventions
//!
//! - Every function returns an `int32_t` status; [`FMF_OK`] is success and the
//!   rest of the table is [`fmf_contract::codes`]. Outputs travel through
//!   out-pointers, which are initialized before any fallible validation so a
//!   failed call never leaves the caller reading uninitialized memory.
//! - Every entry point runs inside a `catch_unwind` guard: a panic becomes
//!   [`FMF_E_PANIC`] instead of unwinding into foreign frames. Detail text for
//!   the *most recent* call on the *calling thread* is retrieved with
//!   [`error::fmf_last_error`].
//! - Strings are UTF-8 in and WTF-8 out: NTFS names may contain unpaired
//!   surrogates, which are preserved as their 3-byte encodings for the host to
//!   decode back to UTF-16 (see `fmf_core::wtf8`).
//! - Engine and result handles are opaque monotonic IDs transported in pointer
//!   position; engine-owned pages and blobs carry a separate monotonic
//!   `owner_id` (ADR-0043). Rust never reconstructs
//!   ownership from a foreign address, and IDs are never reused.
//! - All functions are thread-safe. Re-entering the FFI from inside an event
//!   callback is allowed *except* for the two lifecycle mutations that would
//!   self-deadlock waiting on that very callback,
//!   [`handle::fmf_engine_destroy`] and [`events::fmf_set_event_callback`];
//!   both reject it with [`FMF_E_INVALID_ARG`].
//!
//! # Pointer and length contract (the caller's responsibility)
//!
//! Each function documents its own `# Safety` preconditions. What no
//! function can state in its signature — and what C cannot check — is that a
//! *length claim* is only as trustworthy as its caller:
//!
//! - `(buf, cap)` output arrays ([`volumes::fmf_list_volumes`],
//!   [`volumes::fmf_index_status`]): `buf` must address `cap` writable
//!   `FmfVolumeStatus` values. The engine writes at most `cap` of them and
//!   reports the true total through `count`, so `buf = NULL` is a size probe
//!   that writes `count` only.
//! - `(volumes, n)` input array ([`volumes::fmf_index_start`]): `volumes` must
//!   address `n` readable `char*`, each pointing at NUL-terminated UTF-8.
//! - `(buf, len)` in/out buffer ([`error::fmf_last_error`]): `len` carries the
//!   capacity in and the required byte count (excluding the NUL) out. Too
//!   small a buffer fails without a partial write, never a truncated one.
//! - POD pointers (`FmfQueryOptions*`, `FmfVolumeStatus*`, `FmfEvent*`, …)
//!   must match the declared `#[repr(C)]` size and alignment, which
//!   `fmf-contract` pins with compile-time `offset_of` assertions and
//!   gen-contract radiates into the C# explicit layouts.
//!
//! The engine null-checks every pointer and never writes past a stated `cap`,
//! but it **cannot detect a length that overstates the actual allocation** —
//! that is undefined behavior, not an error code. The contract holds because
//! the only production caller, C# `FfiEngineClient`, constructs each array
//! together with the length it passes, as one unit.
//!
//! # Intentionally absent
//!
//! - `fmf_entry_full_path`. A page row already carries both its name and its
//!   parent path, so a per-entry path call would add a round trip and a second
//!   ownership lifetime to rebuild a string the caller can concatenate.
//! - [`handle::fmf_flush`] has no pipe opcode. Letting a client force a
//!   full snapshot write of every dirty volume is a denial-of-service lever;
//!   the service flushes on its own schedule and at stop.

mod allocation;
/// Heap-allocated JSON blob exchanged with C# (engine stats) and its free function.
pub mod blob;
/// Per-thread last-error storage and the panic/error guard wrapping every entry point.
pub mod error;
/// Engine-event callback registration and the C-ABI event struct delivered to C#.
pub mod events;
/// Engine handle lifecycle: ABI version, create, flush and destroy.
pub mod handle;
mod opaque;
/// Pre-registered cooperative query-cancellation controls.
pub mod query_control;
/// Query execution and paged result retrieval, plus their free functions.
pub mod results;
/// Volume enumeration, indexing start and indexing-status queries.
pub mod volumes;

pub use blob::FmfBlob;
pub use events::{
    FMF_EVENT_ENGINE_ERROR, FMF_EVENT_INDEX_CHANGED, FMF_EVENT_PROGRESS, FMF_EVENT_RESCAN_STARTED,
    FMF_EVENT_VOLUME_FAILED, FMF_EVENT_VOLUME_READY, FmfEvent, FmfEventCb,
};
pub use results::{FmfPage, FmfQueryOptions, FmfRow};
pub use volumes::FmfVolumeStatus;

// Status codes radiate from the contract (ADR-0018); the FMF_-prefixed
// names are this crate's public Rust spelling of the same table.
pub use fmf_contract::codes::{
    CANCELLED as FMF_E_CANCELLED, INVALID_ARG as FMF_E_INVALID_ARG, IO as FMF_E_IO,
    LOCKED as FMF_E_LOCKED, NOT_ADMIN as FMF_E_NOT_ADMIN, OK as FMF_OK, PANIC as FMF_E_PANIC,
    QUERY_SYNTAX as FMF_E_QUERY_SYNTAX, STALE as FMF_E_STALE, VOLUME as FMF_E_VOLUME,
};

#[cfg(test)]
mod contract_tests;

#[cfg(test)]
mod export_pins {
    //! Every extern "C" function pinned by name and signature — a deleted or
    //! re-typed export fails this build before the C# side can crash at runtime.
    use std::ffi::{c_char, c_void};

    use crate::blob::FmfBlob;
    use crate::events::FmfEventCb;
    use crate::results::{FmfPage, FmfQueryOptions};
    use crate::volumes::FmfVolumeStatus;

    #[test]
    fn all_exports_exist() {
        let _: extern "C" fn() -> u32 = crate::handle::fmf_abi_version;
        let _: unsafe extern "C" fn(*const c_char, *mut *mut c_void) -> i32 =
            crate::handle::fmf_engine_create;
        let _: extern "C" fn(*mut c_void) -> i32 = crate::handle::fmf_engine_destroy;
        let _: extern "C" fn(*mut c_void) -> i32 = crate::handle::fmf_flush;
        let _: unsafe extern "C" fn(*mut c_void, FmfEventCb, *mut c_void) -> i32 =
            crate::events::fmf_set_event_callback;
        let _: unsafe extern "C" fn(*mut c_void, *mut FmfVolumeStatus, u32, *mut u32) -> i32 =
            crate::volumes::fmf_list_volumes;
        let _: unsafe extern "C" fn(*mut c_void, *const *const c_char, u32) -> i32 =
            crate::volumes::fmf_index_start;
        let _: unsafe extern "C" fn(*mut c_void, *mut FmfVolumeStatus, u32, *mut u32) -> i32 =
            crate::volumes::fmf_index_status;
        let _: extern "C" fn(u64) -> i32 = crate::blob::fmf_blob_free;
        let _: unsafe extern "C" fn(*mut c_void, *mut *mut FmfBlob) -> i32 =
            crate::blob::fmf_engine_stats;
        let _: unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const FmfQueryOptions,
            u64,
            *mut *mut c_void,
            *mut u64,
            *mut *mut FmfBlob,
        ) -> i32 = crate::results::fmf_query;
        let _: unsafe extern "C" fn(*mut c_void, *mut u64) -> i32 =
            crate::query_control::fmf_query_control_create;
        let _: extern "C" fn(u64) -> i32 = crate::query_control::fmf_query_control_cancel;
        let _: extern "C" fn(u64) -> i32 = crate::query_control::fmf_query_control_free;
        let _: unsafe extern "C" fn(*mut c_void, u64, u32, *mut *mut FmfPage) -> i32 =
            crate::results::fmf_result_page;
        let _: extern "C" fn(u64) -> i32 = crate::results::fmf_page_free;
        let _: extern "C" fn(*mut c_void) -> i32 = crate::results::fmf_result_free;
        let _: unsafe extern "C" fn(*mut u8, *mut u32) -> i32 = crate::error::fmf_last_error;
    }
}
