//! Opcode → Engine mapping (docs/ARCHITECTURE.md「Pipe プロトコル」
//! §オペコード表 — the canonical table; this is its server half). Every
//! request runs inside a catch_unwind firewall: a panic answers FMF_E_PANIC
//! and the connection survives, mirroring the FFI `guard`.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fmf_core::diag::error_chain;
use fmf_core::engine::{Engine, EngineError, ResultSet};
use fmf_core::query::QueryOptions;
use fmf_proto::limits::MAX_RESULTS_PER_CONN;
use fmf_proto::messages::{self, opcode};
use fmf_proto::{ABI_VERSION, PROTOCOL_VERSION, codes};
use parking_lot::Mutex;

use crate::faults::Faults;

struct ResultEntry {
    set: ResultSet,
    last_used: u64,
    /// `!!lag` fault: page fetches on this result sleep 250ms.
    lagged: bool,
}

pub struct Connection {
    pub engine: Arc<Engine>,
    pub faults: Faults,
    results: Mutex<HashMap<u64, ResultEntry>>,
    next_result_id: AtomicU64,
    use_clock: AtomicU64,
    pub hello_done: AtomicBool,
    /// Live-connection count shared with the accept loop (ServiceInfo
    /// reports it; the server owns increment/decrement).
    active_connections: Arc<std::sync::atomic::AtomicUsize>,
}

/// What the worker should do after answering (or instead of answering).
pub enum Outcome {
    /// Send (status, payload) back with FLAG_RESPONSE.
    Reply(i32, Vec<u8>),
    /// Subscribe/Unsubscribe handled by the caller (owns the queue), then
    /// reply OK with an empty payload.
    Subscribe,
    Unsubscribe,
    /// Protocol violation or `!!drop` fault — tear the connection down.
    Drop,
}

impl Connection {
    pub fn new(
        engine: Arc<Engine>,
        faults: Faults,
        active_connections: Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            engine,
            faults,
            results: Mutex::new(HashMap::new()),
            next_result_id: AtomicU64::new(1),
            use_clock: AtomicU64::new(0),
            hello_done: AtomicBool::new(false),
            active_connections,
        }
    }

    pub fn dispatch(&self, op: u16, payload: &[u8]) -> Outcome {
        let result = catch_unwind(AssertUnwindSafe(|| self.dispatch_inner(op, payload)));
        match result {
            Ok(outcome) => outcome,
            Err(_) => Outcome::Reply(
                codes::PANIC,
                b"panic inside fmf-service dispatch \xe2\x80\x94 engine.log".to_vec(),
            ),
        }
    }

    fn dispatch_inner(&self, op: u16, payload: &[u8]) -> Outcome {
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
                self.reply_json("ListVolumes", &vols)
            }
            opcode::INDEX_START => {
                match messages::decode_json::<messages::IndexStartReq>("IndexStart", payload) {
                    Ok(req) => {
                        self.engine.index_start(&req.volumes);
                        Outcome::Reply(codes::OK, Vec::new())
                    }
                    Err(e) => Outcome::Reply(codes::INVALID_ARG, e.to_string().into_bytes()),
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
                self.reply_json("IndexStatus", &status)
            }
            opcode::QUERY => self.query(payload),
            opcode::RESULT_PAGE => self.result_page(payload),
            opcode::RESULT_FREE => match messages::decode_result_free(payload) {
                Ok(id) => {
                    self.results.lock().remove(&id);
                    Outcome::Reply(codes::OK, Vec::new())
                }
                Err(_) => Outcome::Drop,
            },
            opcode::STATS => self.reply_json("Stats", &self.engine.metrics_snapshot()),
            opcode::FLUSH_RESERVED => Outcome::Reply(
                codes::INVALID_ARG,
                b"Flush is reserved and unimplemented on the pipe (ARCHITECTURE.md op 11)".to_vec(),
            ),
            opcode::SERVICE_INFO => self.reply_json(
                "ServiceInfo",
                &messages::ServiceInfoResp {
                    uptime_ms: self.faults.uptime_ms(),
                    connections: self.active_connections.load(Ordering::Relaxed) as u32,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
            ),
            _ => Outcome::Drop,
        }
    }

    fn reply_json<T: serde::Serialize>(&self, what: &'static str, v: &T) -> Outcome {
        match messages::encode_json(what, v) {
            Ok(bytes) => Outcome::Reply(codes::OK, bytes),
            Err(e) => Outcome::Reply(codes::IO, e.to_string().into_bytes()),
        }
    }

    fn query(&self, payload: &[u8]) -> Outcome {
        let Ok((opt, text)) = messages::decode_query_req(payload) else {
            return Outcome::Drop;
        };
        if let Some(outcome) = self.faults.on_query(text) {
            return outcome;
        }
        let q: QueryOptions = opt.into();
        match self.engine.query(text, &q) {
            Ok((set, trace)) => {
                let count = set.len() as u64;
                let id = self.next_result_id.fetch_add(1, Ordering::Relaxed);
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
                        set,
                        last_used: self.use_clock.fetch_add(1, Ordering::Relaxed),
                        lagged,
                    },
                );
                // 黙らない: a trace serialization failure is counted and
                // warned; the query itself succeeded, so the client gets
                // its result with an (explicitly) empty trace.
                let trace_json = match serde_json::to_vec(&trace) {
                    Ok(v) => v,
                    Err(e) => {
                        fmf_core::degrade!(
                            self.engine.metrics().counters.trace_serialize_failures,
                            error = %e,
                            "query trace serialization failed — replying with an empty trace"
                        );
                        Vec::new()
                    }
                };
                Outcome::Reply(
                    codes::OK,
                    messages::QueryRespHead {
                        result_id: id,
                        count,
                    }
                    .encode_with_trace(&trace_json),
                )
            }
            Err(e @ (EngineError::Parse(_) | EngineError::Compile(_))) => {
                Outcome::Reply(codes::QUERY_SYNTAX, error_chain(&e).into_bytes())
            }
            Err(e) => Outcome::Reply(codes::STALE, error_chain(&e).into_bytes()),
        }
    }

    fn result_page(&self, payload: &[u8]) -> Outcome {
        let Ok(req) = messages::ResultPageReq::decode(payload) else {
            return Outcome::Drop;
        };
        let page = {
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
            if entry.lagged {
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            // Row+blob packing is fmf-core's single implementation
            // (ResultSet::fill_page); this layer only frames it.
            entry.set.fill_page(req.offset as usize, req.count as usize)
        };
        match page {
            Ok((rows, blob)) => Outcome::Reply(codes::OK, messages::encode_page(&rows, &blob)),
            Err(EngineError::Stale) => Outcome::Reply(
                codes::STALE,
                b"structural generation moved; re-run the query".to_vec(),
            ),
            Err(e) => Outcome::Reply(codes::IO, e.to_string().into_bytes()),
        }
    }
}
