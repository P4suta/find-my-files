//! Opcode → Engine mapping (docs/ARCHITECTURE.md "Pipe protocol"
//! §opcode table — the canonical table; this is its server half).
//!
//! Every request runs inside a `catch_unwind` firewall: a panic answers
//! `FMF_E_PANIC` and the connection survives, mirroring the FFI `guard`.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fmf_core::diag::error_chain;
use fmf_core::engine::{Engine, EngineError, QueryCancellation, ResultSet};
use fmf_core::query::QueryOptions;
use fmf_proto::limits::MAX_RESULTS_PER_CONN;
use fmf_proto::messages::{self, opcode};
use fmf_proto::{ABI_VERSION, PROTOCOL_VERSION, codes};
use parking_lot::Mutex;

use crate::faults::Faults;

struct ResultEntry {
    /// `Arc` so `result_page` can clone the handle under the results lock and
    /// materialize the page *outside* it (`fill_page` read-locks volume slots);
    /// the map stays free for the connection's other worker meanwhile.
    set: Arc<ResultSet>,
    last_used: u64,
    /// `!!lag` fault: page fetches on this result sleep 250ms.
    lagged: bool,
}

// Result IDs are process-global and monotonic. Per-connection registries
// still own lifetime, while global uniqueness means an opaque handle copied
// from another connection can never alias a local result with the same
// small integer (ADR-0044).
static NEXT_RESULT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct QueryRegistry {
    entries: Mutex<HashMap<u32, QueryCancellation>>,
}

impl QueryRegistry {
    fn begin(&self, request_id: u32) -> QueryCancellation {
        let cancellation = QueryCancellation::new();
        let mut entries = self.entries.lock();
        for previous in entries.values() {
            previous.cancel();
        }
        entries.clear();
        entries.insert(request_id, cancellation.clone());
        cancellation
    }

    fn cancel(&self, request_id: u32) {
        if let Some(cancellation) = self.entries.lock().get(&request_id) {
            cancellation.cancel();
        }
    }

    fn finish(&self, request_id: u32, cancellation: &QueryCancellation) {
        let mut entries = self.entries.lock();
        if entries
            .get(&request_id)
            .is_some_and(|current| current.is_same_query(cancellation))
        {
            entries.remove(&request_id);
        }
    }

    fn cancel_all(&self) {
        let mut entries = self.entries.lock();
        for cancellation in entries.values() {
            cancellation.cancel();
        }
        entries.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().len()
    }
}

/// Per-connection dispatch state: owns the live result handles and the
/// hello/version handshake for one pipe client.
pub struct Connection {
    /// Shared engine all opcodes route into (the only place logic lives).
    pub engine: Arc<Engine>,
    /// Debug fault injector (`!!panic` / `!!drop` / `!!lag`); a no-op for
    /// installed services.
    pub faults: Faults,
    results: Mutex<HashMap<u64, ResultEntry>>,
    use_clock: AtomicU64,
    queries: QueryRegistry,
    /// True once a valid Hello with a matching protocol version arrived;
    /// any other opcode before that is a protocol violation (Drop).
    pub hello_done: AtomicBool,
    /// Live-connection count shared with the accept loop (`ServiceInfo`
    /// reports it; the server owns increment/decrement).
    active_connections: Arc<std::sync::atomic::AtomicUsize>,
}

/// What the worker should do after answering (or instead of answering).
pub enum Outcome {
    /// Send (status, payload) back with `FLAG_RESPONSE`.
    Reply(i32, Vec<u8>),
    /// Subscribe/Unsubscribe handled by the caller (owns the queue), then
    /// reply OK with an empty payload.
    Subscribe,
    /// Unsubscribe handled by the caller (owns the queue), then reply OK
    /// with an empty payload.
    Unsubscribe,
    /// Protocol violation or `!!drop` fault — tear the connection down.
    Drop,
}

impl Connection {
    /// Create a fresh connection bound to the shared engine, fault injector,
    /// and the accept loop's live-connection counter; starts pre-handshake.
    pub fn new(
        engine: Arc<Engine>,
        faults: Faults,
        active_connections: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            engine,
            faults,
            results: Mutex::new(HashMap::new()),
            use_clock: AtomicU64::new(0),
            queries: QueryRegistry::default(),
            hello_done: AtomicBool::new(false),
            active_connections,
        }
    }

    /// Dispatch with the cancellation lifecycle registered by the pipe
    /// reader before the request entered the worker queue.
    pub(crate) fn dispatch_with_query(
        &self,
        op: u16,
        request_id: u32,
        payload: &[u8],
        cancellation: Option<&QueryCancellation>,
    ) -> Outcome {
        let _qid = tracing::info_span!("req", qid = request_id).entered();
        let result = catch_unwind(AssertUnwindSafe(|| {
            self.dispatch_inner(op, payload, cancellation)
        }));
        match result {
            Ok(outcome) => outcome,
            Err(_) => Outcome::Reply(
                codes::PANIC,
                b"panic inside fmf-service dispatch \xe2\x80\x94 engine.log".to_vec(),
            ),
        }
    }

    fn dispatch_inner(
        &self,
        op: u16,
        payload: &[u8],
        cancellation: Option<&QueryCancellation>,
    ) -> Outcome {
        // Hello must come first; anything else on a fresh connection is a
        // protocol violation.
        if !self.hello_done.load(Ordering::Relaxed) && op != opcode::HELLO {
            return Outcome::Drop;
        }
        match op {
            opcode::HELLO => match messages::HelloReq::decode(payload) {
                Ok(req) if req.protocol_version == PROTOCOL_VERSION => {
                    self.hello_done.store(true, Ordering::Relaxed);
                    Outcome::Reply(
                        codes::OK,
                        messages::HelloResp {
                            protocol_version: PROTOCOL_VERSION,
                            abi_version: ABI_VERSION,
                            server_pid: std::process::id(),
                        }
                        .encode(),
                    )
                }
                Ok(req) => {
                    tracing::warn!(
                        client = req.protocol_version,
                        server = PROTOCOL_VERSION,
                        "pipe protocol version mismatch"
                    );
                    Outcome::Reply(
                        codes::INVALID_ARG,
                        format!(
                            "protocol version mismatch: client {} vs server {PROTOCOL_VERSION}",
                            req.protocol_version
                        )
                        .into_bytes(),
                    )
                }
                Err(_) => Outcome::Drop,
            },
            opcode::SUBSCRIBE => Outcome::Subscribe,
            opcode::UNSUBSCRIBE => Outcome::Unsubscribe,
            opcode::LIST_VOLUMES => {
                let vols: Vec<_> = Engine::list_ntfs_volumes()
                    .into_iter()
                    .map(|v| messages::VolumeStatusWire {
                        volume: v,
                        state: 0,
                        entries: 0,
                    })
                    .collect();
                Self::reply_json("ListVolumes", &vols)
            }
            opcode::INDEX_START => {
                match messages::decode_json::<messages::IndexStartReq>("IndexStart", payload) {
                    Ok(req) => {
                        if req.volumes.len() > fmf_proto::limits::MAX_VOLUMES as usize {
                            return Outcome::Reply(
                                codes::INVALID_ARG,
                                format!(
                                    "volume count {} exceeds the contract maximum {}",
                                    req.volumes.len(),
                                    fmf_proto::limits::MAX_VOLUMES
                                )
                                .into_bytes(),
                            );
                        }
                        match self.engine.index_start(&req.volumes) {
                            Ok(()) => Outcome::Reply(codes::OK, Vec::new()),
                            Err(error) => {
                                tracing::warn!(%error, "IndexStart volume selection rejected");
                                Outcome::Reply(
                                    codes::INVALID_ARG,
                                    b"invalid IndexStart volume selection".to_vec(),
                                )
                            }
                        }
                    }
                    // The serde detail (field names, byte offsets) is internal
                    // shape — log it for F12/engine.log, hand the client a
                    // generic verdict rather than echoing our payload layout.
                    Err(e) => {
                        tracing::warn!(error = %e, "IndexStart payload rejected");
                        Outcome::Reply(codes::INVALID_ARG, b"malformed IndexStart payload".to_vec())
                    }
                }
            }
            opcode::INDEX_STATUS => {
                let status: Vec<_> = self
                    .engine
                    .status()
                    .into_iter()
                    .map(|(volume, phase, entries)| messages::VolumeStatusWire {
                        volume,
                        // VolumeState is the contract enum (repr u32).
                        state: phase as u32,
                        entries,
                    })
                    .collect();
                Self::reply_json("IndexStatus", &status)
            }
            opcode::QUERY => {
                let Some(cancellation) = cancellation else {
                    return Outcome::Reply(
                        codes::INVALID_ARG,
                        b"query cancellation lifecycle is missing".to_vec(),
                    );
                };
                self.query(payload, cancellation)
            }
            opcode::RESULT_PAGE => self.result_page(payload),
            opcode::RESULT_FREE => match messages::decode_result_free(payload) {
                Ok(id) => {
                    self.results.lock().remove(&id);
                    Outcome::Reply(codes::OK, Vec::new())
                }
                Err(_) => Outcome::Drop,
            },
            opcode::STATS => Self::reply_json("Stats", &self.engine.metrics_snapshot()),
            opcode::SERVICE_INFO => Self::reply_json(
                "ServiceInfo",
                &messages::ServiceInfoResp {
                    uptime_ms: self.faults.uptime_ms(),
                    connections: self.active_connections.load(Ordering::Relaxed) as u32,
                    version: fmf_buildstamp::VERSION.to_string(),
                },
            ),
            _ => Outcome::Drop,
        }
    }

    fn reply_json<T: serde::Serialize>(what: &'static str, v: &T) -> Outcome {
        match messages::encode_json(what, v) {
            Ok(bytes) => Outcome::Reply(codes::OK, bytes),
            Err(e) => Outcome::Reply(codes::IO, e.to_string().into_bytes()),
        }
    }

    /// Register a query before it enters the bounded work queue. Every older
    /// queued/running query is cancelled first (latest-query-wins).
    pub(crate) fn begin_query(&self, request_id: u32) -> QueryCancellation {
        self.queries.begin(request_id)
    }

    /// Cancel one live/queued query. Unknown IDs are an idempotent no-op on
    /// this one-way control path.
    pub(crate) fn cancel_query(&self, request_id: u32) {
        self.queries.cancel(request_id);
    }

    /// Remove a completed request without letting an old reused request ID
    /// erase a newer lifecycle.
    pub(crate) fn finish_query(&self, request_id: u32, cancellation: &QueryCancellation) {
        self.queries.finish(request_id, cancellation);
    }

    /// Cancel and forget all requests on disconnect.
    pub(crate) fn cancel_all_queries(&self) {
        self.queries.cancel_all();
    }

    fn query(&self, payload: &[u8], cancellation: &QueryCancellation) -> Outcome {
        let Ok((opt, text)) = messages::decode_query_req(payload) else {
            return Outcome::Drop;
        };
        if let Some(outcome) = self.faults.on_query(text) {
            return outcome;
        }
        let q = match QueryOptions::try_from(opt) {
            Ok(options) => options,
            Err(e) => return Outcome::Reply(codes::INVALID_ARG, e.to_string().into_bytes()),
        };
        let basis = if opt.presentation_basis == 0 {
            None
        } else {
            let mut results = self.results.lock();
            results.get_mut(&opt.presentation_basis).map(|entry| {
                entry.last_used = self.use_clock.fetch_add(1, Ordering::Relaxed);
                Arc::clone(&entry.set)
            })
        };
        match self
            .engine
            .query_cancellable(text, &q, cancellation, basis.as_deref())
        {
            Ok((set, mut trace)) => {
                if cancellation.is_cancelled() {
                    return Outcome::Reply(codes::CANCELLED, b"query cancelled".to_vec());
                }
                // The basis must remain registered through completion. A
                // concurrent ResultFree/eviction turns unchanged off.
                if let Some(basis) = basis.as_ref() {
                    trace.unchanged &= self
                        .results
                        .lock()
                        .get(&opt.presentation_basis)
                        .is_some_and(|entry| Arc::ptr_eq(&entry.set, basis));
                }
                let count = set.len() as u64;
                let id = NEXT_RESULT_ID.fetch_add(1, Ordering::Relaxed);
                if id == 0 {
                    return Outcome::Reply(
                        codes::IO,
                        b"result handle namespace exhausted".to_vec(),
                    );
                }
                fmf_core::diag::log_query_served(id, &trace);
                let lagged = self.faults.lag_marker(text);
                let mut results = self.results.lock();
                if results.len() >= MAX_RESULTS_PER_CONN {
                    // Evict the least recently *used* (not oldest-created):
                    // the on-screen result survives query bursts.
                    if let Some((&victim, _)) = results.iter().min_by_key(|(_, e)| e.last_used) {
                        results.remove(&victim);
                        fmf_core::degrade!(
                            self.engine.metrics().counters.pipe_results_evicted,
                            result_id = victim,
                            "result handle LRU-evicted at the per-connection cap"
                        );
                    }
                }
                results.insert(
                    id,
                    ResultEntry {
                        set: Arc::new(set),
                        last_used: self.use_clock.fetch_add(1, Ordering::Relaxed),
                        lagged,
                    },
                );
                // One buffer: the 16-byte head, then the trace JSON appended in
                // place (no intermediate Vec + copy). don't go silent: on a
                // serialize failure, truncate back to the head and reply with an
                // (explicitly) empty trace, counted + warned — the query itself
                // succeeded.
                let mut reply = messages::QueryRespHead {
                    result_id: id,
                    count,
                }
                .begin_response(256);
                if let Err(e) = serde_json::to_writer(&mut reply, &trace) {
                    reply.truncate(messages::QueryRespHead::LEN);
                    fmf_core::degrade!(
                        self.engine.metrics().counters.trace_serialize_failures,
                        error = %e,
                        "query trace serialization failed — replying with an empty trace"
                    );
                }
                Outcome::Reply(codes::OK, reply)
            }
            Err(e @ (EngineError::Parse(_) | EngineError::Compile(_))) => {
                Outcome::Reply(codes::QUERY_SYNTAX, error_chain(&e).into_bytes())
            }
            Err(e @ EngineError::QueryTooLong { .. }) => {
                Outcome::Reply(codes::INVALID_ARG, e.to_string().into_bytes())
            }
            Err(EngineError::Cancelled) => {
                Outcome::Reply(codes::CANCELLED, b"query cancelled".to_vec())
            }
            Err(e) => Outcome::Reply(codes::STALE, error_chain(&e).into_bytes()),
        }
    }

    fn result_page(&self, payload: &[u8]) -> Outcome {
        let Ok(req) = messages::ResultPageReq::decode(payload) else {
            return Outcome::Drop;
        };
        // Clone the result handle under the lock, then materialize the page
        // OUTSIDE it: fill_page read-locks N volume slots and builds the
        // rows+blob, work that must not pin the per-connection results map
        // (the other worker can page a different result, free one, or insert a
        // new query meanwhile). The cloned Arc keeps the set alive even if it
        // is evicted/freed mid-fill.
        let (set, lagged) = {
            let mut results = self.results.lock();
            let Some(entry) = results.get_mut(&req.result_id) else {
                // Evicted (or never existed): the client recovers through
                // its STALE → re-query path; "evicted" keeps F12 honest.
                return Outcome::Reply(
                    codes::STALE,
                    b"result handle evicted or unknown \xe2\x80\x94 re-run the query".to_vec(),
                );
            };
            entry.last_used = self.use_clock.fetch_add(1, Ordering::Relaxed);
            (Arc::clone(&entry.set), entry.lagged)
        };
        if lagged {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        // Row+blob packing is fmf-core's single implementation
        // (ResultSet::fill_page); this layer only frames it.
        let Ok(offset) = usize::try_from(req.offset) else {
            return Outcome::Reply(
                codes::INVALID_ARG,
                b"result page offset exceeds the supported address space".to_vec(),
            );
        };
        match set.fill_page(offset, req.count as usize) {
            Ok((rows, blob)) => match messages::encode_page(&rows, &blob) {
                Ok(payload) => Outcome::Reply(codes::OK, payload),
                Err(error) => Outcome::Reply(codes::IO, error.to_string().into_bytes()),
            },
            Err(EngineError::Stale) => Outcome::Reply(
                codes::STALE,
                b"structural generation moved; re-run the query".to_vec(),
            ),
            Err(e @ EngineError::PageTooLarge { .. }) => {
                Outcome::Reply(codes::INVALID_ARG, e.to_string().into_bytes())
            }
            Err(e) => Outcome::Reply(codes::IO, e.to_string().into_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QueryRegistry;

    #[test]
    fn latest_query_cancels_running_or_queued_predecessor() {
        let registry = QueryRegistry::default();
        let older = registry.begin(10);
        let newest = registry.begin(11);
        assert!(older.is_cancelled());
        assert!(!newest.is_cancelled());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn explicit_cancel_is_idempotent_and_scoped_to_request_id() {
        let registry = QueryRegistry::default();
        let query = registry.begin(20);
        registry.cancel(999);
        assert!(!query.is_cancelled());
        registry.cancel(20);
        registry.cancel(20);
        assert!(query.is_cancelled());
        assert_eq!(registry.len(), 1, "worker cleanup owns removal");
    }

    #[test]
    fn old_worker_cleanup_cannot_remove_reused_request_id() {
        let registry = QueryRegistry::default();
        let old = registry.begin(7);
        let current = registry.begin(7);
        registry.finish(7, &old);
        assert_eq!(registry.len(), 1);
        assert!(!current.is_cancelled());
        registry.finish(7, &current);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn disconnect_cancels_all_without_leaked_ids() {
        let registry = QueryRegistry::default();
        let query = registry.begin(42);
        registry.cancel_all();
        assert!(query.is_cancelled());
        assert_eq!(registry.len(), 0);
    }
}
