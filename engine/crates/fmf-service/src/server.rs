//! Pipe server: accept loop (2-wait on connect/stop) + per-connection threads.
//!
//! One reader decodes frames into a small queue, two workers dispatch
//! out-of-order (a page fetch never queues behind a slow query), all frame
//! writes — responses and event pushes — serialize on one mutex so frames
//! can't interleave mid-stream.

use std::io;
use std::sync::Arc;
use std::sync::mpsc;

use fmf_core::engine::Engine;
use fmf_core::metrics::Counters;
use fmf_proto::frame::{self, FLAG_EVENT, FLAG_RESPONSE, FrameError, FrameHeader};
use fmf_proto::messages;
use parking_lot::Mutex;

use crate::dispatch::{Connection, Outcome};
use crate::events::Broadcaster;
use crate::faults::Faults;
use crate::pipe::{Accepted, Event, PipeListener, PipeStream};

/// Max concurrent pipe instances the listener will create; further clients
/// hit `ERROR_PIPE_BUSY` at the OS until a slot frees.
pub const MAX_INSTANCES: u32 = 8;
const WORKERS_PER_CONNECTION: usize = 2;

/// Configuration for starting the pipe [`Server`].
pub struct ServerOptions {
    /// Named-pipe name the listener binds and accepts connections on.
    pub pipe_name: String,
    /// Enable debug fault injection (`!!panic` / `!!drop` / `!!lag`); always
    /// off for the installed service.
    pub debug_faults: bool,
    /// Connect-time token allowlist (docs/SECURITY.md layer 4 of the 4-layer
    /// defense). Empty =
    /// no check (console/test mode); the installed service always fills it.
    pub authorized_sids: Vec<String>,
    /// Data root for the machine-wide `last_use` stamp (ADR-0027): each accepted
    /// connection refreshes it so the GC ages out only a genuinely unused install.
    pub data_dir: std::path::PathBuf,
}

/// Running pipe server: owns the accept thread and its stop event.
pub struct Server {
    stop: Arc<Event>,
    active: Arc<std::sync::atomic::AtomicUsize>,
    accept_thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// # Errors
    /// Returns the OS error if the stop event cannot be created.
    ///
    /// # Panics
    /// Panics if the accept thread fails to spawn.
    pub fn start(engine: Arc<Engine>, opts: ServerOptions) -> io::Result<Arc<Self>> {
        let stop = Arc::new(Event::new()?);
        let broadcaster = Broadcaster::install(&engine);
        // Live-connection count: incremented per accepted connection, freed by
        // the per-connection guard when its thread exits. Held by the Server so
        // serve()'s idle self-stop (ADR-0027) can read it.
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accept_stop = stop.clone();
        let accept_active = active.clone();
        let accept_thread = std::thread::Builder::new()
            .name("fmf-pipe-accept".to_string())
            .spawn(move || {
                accept_loop(engine, broadcaster, opts, &accept_stop, accept_active);
            })
            .expect("spawn accept thread");
        Ok(Arc::new(Self {
            stop,
            active,
            accept_thread: Some(accept_thread),
        }))
    }

    /// Stops accepting new connections. Live connections end with their
    /// clients (the engine is flushed/shut down by the caller afterwards).
    pub fn stop(&self) {
        self.stop.set();
    }

    /// Live pipe-connection count — drives `serve()`'s idle self-stop (ADR-0027).
    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Blocks until the accept thread has exited (call after [`Server::stop`]).
    pub fn join(mut self: Arc<Self>) {
        if let Some(s) = Arc::get_mut(&mut self)
            && let Some(t) = s.accept_thread.take()
        {
            let _ = t.join();
        }
    }
}

fn accept_loop(
    engine: Arc<Engine>,
    broadcaster: Arc<Broadcaster>,
    opts: ServerOptions,
    stop: &Event,
    active: Arc<std::sync::atomic::AtomicUsize>,
) {
    let security = if opts.authorized_sids.is_empty() {
        None
    } else {
        match crate::security::PipeSecurity::from_sddl(&crate::security::pipe_sddl(
            &opts.authorized_sids,
        )) {
            Ok(s) => Some(s),
            Err(e) => {
                // Refusing to serve wide-open beats serving wide-open.
                tracing::error!(error = %e, "pipe SDDL conversion failed — not serving");
                return;
            }
        }
    };
    let mut listener = PipeListener::new(&opts.pipe_name, MAX_INSTANCES, security);
    loop {
        match listener.accept(stop) {
            Ok(Accepted::Stopped) => return,
            Ok(Accepted::Connection(stream)) => {
                // Defense in depth behind the DACL: verify the client token.
                if matches!(
                    crate::security::verify_client(&stream, &opts.authorized_sids),
                    Ok(true)
                ) {
                } else {
                    Counters::bump(&engine.metrics().counters.pipe_connections_rejected);
                    tracing::warn!("pipe client token rejected");
                    stream.disconnect();
                    continue;
                }
                // An authorized client connected — refresh the use stamp so the
                // GC ages out only a genuinely unused install (ADR-0027).
                crate::lifecycle::stamp_last_use(&opts.data_dir);
                let engine = engine.clone();
                let broadcaster = broadcaster.clone();
                let faults = Faults::new(opts.debug_faults);
                let active = active.clone();
                std::thread::Builder::new()
                    .name("fmf-pipe-conn".to_string())
                    .spawn(move || run_connection(engine, broadcaster, stream, faults, active))
                    .ok();
            }
            Err(e) => {
                // Typically ERROR_PIPE_BUSY at the instance cap — the
                // client was turned away by the OS; count, breathe, retry.
                Counters::bump(&engine.metrics().counters.pipe_connections_rejected);
                tracing::warn!(error = %e, "pipe accept failed — retrying");
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }
}

fn run_connection(
    engine: Arc<Engine>,
    broadcaster: Arc<Broadcaster>,
    stream: PipeStream,
    faults: Faults,
    active: Arc<std::sync::atomic::AtomicUsize>,
) {
    // Decrement on every exit path (including panics) — the count must
    // never drift from the number of live connection threads.
    struct ActiveGuard(Arc<std::sync::atomic::AtomicUsize>);
    impl Drop for ActiveGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    active.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _guard = ActiveGuard(active.clone());

    let conn = Arc::new(Connection::new(engine.clone(), faults, active));
    let writer = Arc::new(Mutex::new(stream.clone()));
    // (queue handle, event-writer join) — at most one subscription per
    // connection; Subscribe is idempotent.
    let subscription: Arc<Mutex<Option<Arc<crate::events::EventQueue>>>> =
        Arc::new(Mutex::new(None));

    let (tx, rx) = mpsc::channel::<(FrameHeader, Vec<u8>)>();
    let rx = Arc::new(Mutex::new(rx));
    let mut workers = Vec::new();
    for _ in 0..WORKERS_PER_CONNECTION {
        let rx = rx.clone();
        let conn = conn.clone();
        let writer = writer.clone();
        let broadcaster = broadcaster.clone();
        let subscription = subscription.clone();
        let stream = stream.clone();
        workers.push(std::thread::spawn(move || {
            loop {
                let msg = rx.lock().recv();
                let Ok((header, payload)) = msg else { return };
                match conn.dispatch(header.opcode, &payload) {
                    Outcome::Reply(status, body) => {
                        let h = FrameHeader {
                            len: 0,
                            opcode: header.opcode,
                            flags: FLAG_RESPONSE,
                            request_id: header.request_id,
                            status,
                        };
                        if frame::write_frame(&mut *writer.lock(), h, &body).is_err() {
                            return; // client went away; reader notices too
                        }
                    }
                    Outcome::Subscribe => {
                        {
                            let mut sub = subscription.lock();
                            if sub.is_none() {
                                let q = broadcaster.subscribe();
                                spawn_event_writer(q.clone(), writer.clone());
                                *sub = Some(q);
                            }
                        }
                        let h = FrameHeader {
                            len: 0,
                            opcode: header.opcode,
                            flags: FLAG_RESPONSE,
                            request_id: header.request_id,
                            status: 0,
                        };
                        let _ = frame::write_frame(&mut *writer.lock(), h, &[]);
                    }
                    Outcome::Unsubscribe => {
                        if let Some(q) = subscription.lock().take() {
                            broadcaster.unsubscribe(&q);
                        }
                        let h = FrameHeader {
                            len: 0,
                            opcode: header.opcode,
                            flags: FLAG_RESPONSE,
                            request_id: header.request_id,
                            status: 0,
                        };
                        let _ = frame::write_frame(&mut *writer.lock(), h, &[]);
                    }
                    Outcome::Drop => {
                        stream.disconnect();
                        return;
                    }
                }
            }
        }));
    }

    // Reader: the only thread that touches the receive side.
    let mut reader = stream.clone();
    loop {
        match frame::read_frame(&mut reader) {
            Ok((header, payload)) => {
                // Requests must not carry response/event flags.
                if header.flags != 0 {
                    Counters::bump(&engine.metrics().counters.pipe_malformed_frames);
                    tracing::warn!("malformed frame (flags on a request) — dropping connection");
                    stream.disconnect();
                    break;
                }
                if tx.send((header, payload)).is_err() {
                    break; // a worker dropped the connection
                }
            }
            Err(FrameError::TooLong(len)) => {
                Counters::bump(&engine.metrics().counters.pipe_malformed_frames);
                tracing::warn!(len, "oversized frame — dropping connection");
                stream.disconnect();
                break;
            }
            Err(FrameError::Io(_)) => break, // disconnect / shutdown
        }
    }

    drop(tx); // workers drain and exit
    if let Some(q) = subscription.lock().take() {
        broadcaster.unsubscribe(&q); // closes the queue → event writer exits
    }
    for w in workers {
        let _ = w.join();
    }
}

fn spawn_event_writer(q: Arc<crate::events::EventQueue>, writer: Arc<Mutex<PipeStream>>) {
    std::thread::Builder::new()
        .name("fmf-pipe-events".to_string())
        .spawn(move || {
            while let Some(ev) = q.pop() {
                let h = FrameHeader {
                    len: 0,
                    opcode: ev.kind as u16,
                    flags: FLAG_EVENT,
                    request_id: 0,
                    status: 0,
                };
                if frame::write_frame(&mut *writer.lock(), h, &messages::encode_event(&ev)).is_err()
                {
                    return;
                }
            }
        })
        .ok();
}
