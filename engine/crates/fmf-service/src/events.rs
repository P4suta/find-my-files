//! Event fan-out: one engine sink → per-subscriber bounded queues. A slow
//! or dead reader must never block the volume threads that emit events
//! (docs/ARCHITECTURE.md「Pipe プロトコル」§イベントプッシュ): the queue is
//! bounded at 256 and overflow drops the *oldest* event, counted and warned.

use std::collections::VecDeque;
use std::sync::Arc;

use fmf_core::engine::{Engine, EngineEvent};
use fmf_core::metrics::Counters;
use fmf_proto::messages::EventWire;
use parking_lot::{Condvar, Mutex};

const QUEUE_CAP: usize = 256;

/// FFI event kinds (fmf-ffi/src/events.rs values — the wire uses the same).
fn wire_of(ev: &EngineEvent) -> EventWire {
    let (kind, volume, entries) = match ev {
        EngineEvent::Progress { volume, entries } => (1, volume, *entries),
        EngineEvent::VolumeReady { volume, entries } => (2, volume, *entries),
        EngineEvent::IndexChanged { volume } => (3, volume, 0),
        EngineEvent::RescanStarted { volume } => (4, volume, 0),
        EngineEvent::VolumeFailed { volume, .. } => (5, volume, 0),
        EngineEvent::EngineError { severity, volume } => (6, volume, *severity),
    };
    EventWire {
        kind,
        entries,
        volume: EventWire::volume_bytes(volume),
    }
}

struct QueueState {
    items: VecDeque<EventWire>,
    closed: bool,
}

/// One subscriber's bounded queue. The connection's writer thread drains it.
pub struct EventQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl EventQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(QueueState {
                items: VecDeque::new(),
                closed: false,
            }),
            ready: Condvar::new(),
        })
    }

    fn push(&self, ev: EventWire) -> bool {
        let mut s = self.state.lock();
        if s.closed {
            return false;
        }
        let dropped = if s.items.len() >= QUEUE_CAP {
            s.items.pop_front();
            true
        } else {
            false
        };
        s.items.push_back(ev);
        self.ready.notify_one();
        dropped
    }

    /// Blocks until an event arrives or the queue is closed (None).
    pub fn pop(&self) -> Option<EventWire> {
        let mut s = self.state.lock();
        loop {
            if let Some(ev) = s.items.pop_front() {
                return Some(ev);
            }
            if s.closed {
                return None;
            }
            self.ready.wait(&mut s);
        }
    }

    pub fn close(&self) {
        self.state.lock().closed = true;
        self.ready.notify_all();
    }
}

/// Owns the engine sink registration; connections subscribe/unsubscribe.
pub struct Broadcaster {
    subscribers: Mutex<Vec<Arc<EventQueue>>>,
}

impl Broadcaster {
    /// Registers self as the engine's (single) event sink.
    pub fn install(engine: &Arc<Engine>) -> Arc<Self> {
        let b = Arc::new(Self {
            subscribers: Mutex::new(Vec::new()),
        });
        let sink = b.clone();
        // Weak: the sink closure is owned by the engine; a strong Arc here
        // would be a reference cycle keeping the engine alive forever.
        let engine_for_counters = Arc::downgrade(engine);
        engine.set_event_sink(Some(Arc::new(move |ev: &EngineEvent| {
            let wire = wire_of(ev);
            let mut dropped = 0u64;
            for q in sink.subscribers.lock().iter() {
                if q.push(wire) {
                    dropped += 1;
                }
            }
            if dropped > 0 {
                if let Some(e) = engine_for_counters.upgrade() {
                    Counters::add(&e.metrics().counters.pipe_events_dropped, dropped);
                }
                tracing::warn!(dropped, "pipe event queue overflow — oldest dropped");
            }
        })));
        b
    }

    pub fn subscribe(&self) -> Arc<EventQueue> {
        let q = EventQueue::new();
        self.subscribers.lock().push(q.clone());
        q
    }

    pub fn unsubscribe(&self, q: &Arc<EventQueue>) {
        q.close();
        self.subscribers.lock().retain(|s| !Arc::ptr_eq(s, q));
    }
}
