//! Pipe opcodes. Event pushes reuse 1..=6 as the event *kind* with
//! `flags = event` — dispatch must branch on the flag before the opcode.
//!
//! Numbers are append-only; a gap (there is no `11`) stays a gap forever.
//! Each request opcode maps onto the FFI entry point named below. The C-ABI
//! functions with no opcode are absorbed by the transport itself:
//! `fmf_engine_create`/`fmf_engine_destroy` by connection and service
//! lifetime, `fmf_page_free`/`fmf_blob_free` because payload ownership moves
//! to the client on frame receipt, and `fmf_last_error` because an error
//! response carries its detail inline (a thread-local pull has no meaning
//! across processes).

/// `Hello`: connection handshake and version negotiation (maps to `fmf_abi_version`).
pub const HELLO: u16 = 1;
/// `Subscribe`: push events to this connection from now on (maps to `fmf_set_event_callback(cb≠NULL)`).
pub const SUBSCRIBE: u16 = 2;
/// `Unsubscribe`: stop pushing events (maps to `fmf_set_event_callback(NULL)`).
pub const UNSUBSCRIBE: u16 = 3;
/// `ListVolumes`: return the state and entry count of every volume (maps to `fmf_list_volumes`).
pub const LIST_VOLUMES: u16 = 4;
/// `IndexStart`: start indexing the given volumes (maps to `fmf_index_start`).
///
/// The whole request is validated before any side effect, and success is
/// idempotent per volume label. The selection is process-local state of the
/// running engine — nothing about it is persisted.
pub const INDEX_START: u16 = 5;
/// `IndexStatus`: return index progress and state (maps to `fmf_index_status`; same shape as `ListVolumes`).
pub const INDEX_STATUS: u16 = 6;
/// `Query`: run a query and return `result_id` and the entry count (maps to `fmf_query`).
pub const QUERY: u16 = 7;
/// `ResultPage`: fetch a row page from `result_id`'s results (maps to `fmf_result_page`).
pub const RESULT_PAGE: u16 = 8;
/// `ResultFree`: free `result_id`'s result handle (maps to `fmf_result_free`).
pub const RESULT_FREE: u16 = 9;
/// `Stats`: return the engine's metrics snapshot (maps to `fmf_engine_stats`).
pub const STATS: u16 = 10;
/// `ServiceInfo`: return service-specific runtime info (`uptime_ms` / connections / version).
pub const SERVICE_INFO: u16 = 12;
/// `QueryCancel`: one-way control frame. The zero-payload frame's
/// `request_id` identifies the query request to cancel.
pub const QUERY_CANCEL: u16 = 13;
