//! fmf-ffi — C ABI surface over fmf-core (canonical contract:
//! docs/ARCHITECTURE.md). Conversion, handle management and panic catching
//! only; every function maps 1:1 onto a future named-pipe message.

// Safety contracts for every entry point live in docs/ARCHITECTURE.md (the
// canonical FFI contract) rather than per-function doc comments.
#![allow(clippy::missing_safety_doc)]

/// Heap-allocated JSON blob exchanged with C# (engine stats) and its free function.
pub mod blob;
/// Per-thread last-error storage and the panic/error guard wrapping every entry point.
pub mod error;
/// Engine-event callback registration and the C-ABI event struct delivered to C#.
pub mod events;
/// Engine handle lifecycle: ABI version, create, flush and destroy.
pub mod handle;
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
    INVALID_ARG as FMF_E_INVALID_ARG, IO as FMF_E_IO, LOCKED as FMF_E_LOCKED,
    NOT_ADMIN as FMF_E_NOT_ADMIN, OK as FMF_OK, PANIC as FMF_E_PANIC,
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
        let _: unsafe extern "C" fn(*mut c_void) -> i32 = crate::handle::fmf_engine_destroy;
        let _: unsafe extern "C" fn(*mut c_void) -> i32 = crate::handle::fmf_flush;
        let _: unsafe extern "C" fn(*mut c_void, FmfEventCb, *mut c_void) -> i32 =
            crate::events::fmf_set_event_callback;
        let _: unsafe extern "C" fn(*mut c_void, *mut FmfVolumeStatus, u32, *mut u32) -> i32 =
            crate::volumes::fmf_list_volumes;
        let _: unsafe extern "C" fn(*mut c_void, *const *const c_char, u32) -> i32 =
            crate::volumes::fmf_index_start;
        let _: unsafe extern "C" fn(*mut c_void, *mut FmfVolumeStatus, u32, *mut u32) -> i32 =
            crate::volumes::fmf_index_status;
        let _: unsafe extern "C" fn(*mut FmfBlob) -> i32 = crate::blob::fmf_blob_free;
        let _: unsafe extern "C" fn(*mut c_void, *mut *mut FmfBlob) -> i32 =
            crate::blob::fmf_engine_stats;
        let _: unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const FmfQueryOptions,
            *mut *mut c_void,
            *mut u64,
            *mut *mut FmfBlob,
        ) -> i32 = crate::results::fmf_query;
        let _: unsafe extern "C" fn(*mut c_void, u64, u32, *mut *mut FmfPage) -> i32 =
            crate::results::fmf_result_page;
        let _: unsafe extern "C" fn(*mut FmfPage) -> i32 = crate::results::fmf_page_free;
        let _: unsafe extern "C" fn(*mut c_void) -> i32 = crate::results::fmf_result_free;
        let _: unsafe extern "C" fn(*mut u8, *mut u32) -> i32 = crate::error::fmf_last_error;
    }
}
