//! Pipe server: accept loop (2-wait on connect/stop) + per-connection threads.
//!
//! One reader decodes frames into a small queue, two workers dispatch
//! out-of-order (a page fetch never queues behind a slow query), all frame
//! writes — responses and event pushes — serialize on one mutex so frames
//! can't interleave mid-stream.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};
use fmf_core::engine::{Engine, QueryCancellation};
use fmf_core::metrics::Counters;
use fmf_proto::codes;
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
/// Requests decoded ahead of the two per-connection workers.
///
/// One waiting request per worker preserves a small amount of pipelining while
/// bounding decoded request payloads. A cancellation channel interrupts a
/// producer waiting on this bound when the server stops or the pipe breaks.
pub const REQUEST_QUEUE_CAP: usize = WORKERS_PER_CONNECTION;
const CANCELLATION_RECEIVERS: usize = WORKERS_PER_CONNECTION + 1;

struct Request {
    header: FrameHeader,
    payload: Vec<u8>,
    query_cancellation: Option<QueryCancellation>,
}

const fn request_payload_cap(opcode: u16) -> u32 {
    use messages::opcode;

    #[expect(
        clippy::match_same_arms,
        reason = "zero-payload contract operations are enumerated explicitly; unknown opcodes also fail closed at zero"
    )]
    match opcode {
        opcode::HELLO => 4,
        opcode::SUBSCRIBE
        | opcode::UNSUBSCRIBE
        | opcode::LIST_VOLUMES
        | opcode::INDEX_STATUS
        | opcode::STATS
        | opcode::SERVICE_INFO
        | opcode::QUERY_CANCEL => 0,
        opcode::INDEX_START => fmf_proto::limits::MAX_INDEX_START_PAYLOAD_LEN,
        opcode::QUERY => messages::FmfQueryOptions::LEN as u32 + fmf_proto::limits::MAX_QUERY_BYTES,
        opcode::RESULT_PAGE => messages::ResultPageReq::LEN as u32,
        opcode::RESULT_FREE => 8,
        _ => 0,
    }
}

fn request_channel() -> (Sender<Request>, Receiver<Request>) {
    crossbeam_channel::bounded(REQUEST_QUEUE_CAP)
}

fn cancellation_channel() -> (Sender<()>, Receiver<()>) {
    crossbeam_channel::bounded(CANCELLATION_RECEIVERS)
}

fn cancel(cancel_tx: &Sender<()>) {
    // One signal for the reader and one for each worker. Filling an already
    // signalled channel is idempotent, so concurrent failure paths coalesce.
    for _ in 0..CANCELLATION_RECEIVERS {
        if cancel_tx.try_send(()).is_err() {
            break;
        }
    }
}

fn disconnect(cancel_tx: &Sender<()>, stream: &PipeStream) {
    cancel(cancel_tx);
    stream.disconnect();
}

fn dequeue_request(rx: &Receiver<Request>, cancel_rx: &Receiver<()>) -> Option<Request> {
    crossbeam_channel::select_biased! {
        recv(cancel_rx) -> _ => None,
        recv(rx) -> result => result.ok(),
    }
}

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
    connections: Arc<ConnectionRegistry>,
    accept_thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct ConnectionRegistry {
    next_id: AtomicU64,
    connections: Mutex<HashMap<u64, ConnectionControl>>,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

struct ConnectionControl {
    stream: PipeStream,
    cancel_tx: Sender<()>,
}

impl ConnectionRegistry {
    fn register(&self, stream: PipeStream, cancel_tx: Sender<()>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.connections
            .lock()
            .insert(id, ConnectionControl { stream, cancel_tx });
        id
    }

    fn unregister(&self, id: u64) {
        self.connections.lock().remove(&id);
    }

    fn disconnect_all(&self) {
        for connection in self.connections.lock().values() {
            disconnect(&connection.cancel_tx, &connection.stream);
        }
    }

    fn push_thread(&self, thread: std::thread::JoinHandle<()>) {
        let mut threads = self.threads.lock();
        let mut index = 0;
        while index < threads.len() {
            if threads[index].is_finished() {
                let finished = threads.swap_remove(index);
                if finished.join().is_err() {
                    tracing::error!("pipe connection thread panicked");
                }
            } else {
                index += 1;
            }
        }
        threads.push(thread);
    }

    fn join_all(&self) {
        for thread in self.threads.lock().drain(..) {
            if thread.join().is_err() {
                tracing::error!("pipe connection thread panicked");
            }
        }
    }
}

impl Server {
    /// # Errors
    /// Returns the OS error if the stop event, pipe security descriptor, first
    /// pipe instance, or accept thread cannot be created. Success means the
    /// first instance is already listening; callers never observe a
    /// superficially-running server with a dead accept thread.
    pub fn start(engine: Arc<Engine>, opts: ServerOptions) -> io::Result<Arc<Self>> {
        let stop = Arc::new(Event::new()?);
        let broadcaster = Broadcaster::install(&engine);
        // Live-connection count: incremented per accepted connection, freed by
        // the per-connection guard when its thread exits. Held by the Server so
        // serve()'s idle self-stop (ADR-0027) can read it.
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connections = Arc::new(ConnectionRegistry::default());
        let accept_stop = stop.clone();
        let accept_active = active.clone();
        let accept_engine = engine.clone();
        let accept_connections = connections.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let accept_thread = match std::thread::Builder::new()
            .name("fmf-pipe-accept".to_string())
            .spawn(move || {
                accept_loop(
                    accept_engine,
                    broadcaster,
                    opts,
                    &accept_stop,
                    accept_active,
                    accept_connections,
                    ready_tx,
                );
            }) {
            Ok(thread) => thread,
            Err(e) => {
                engine.set_event_sink(None);
                return Err(e);
            }
        };

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Arc::new(Self {
                stop,
                active,
                connections,
                accept_thread: Some(accept_thread),
            })),
            Ok(Err(e)) => {
                engine.set_event_sink(None);
                if accept_thread.join().is_err() {
                    tracing::error!("pipe accept thread panicked after reporting startup failure");
                }
                Err(e)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                stop.set();
                engine.set_event_sink(None);
                if accept_thread.join().is_err() {
                    tracing::error!("pipe accept thread panicked during startup timeout");
                }
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "pipe listener did not become ready within 5 seconds",
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop.set();
                engine.set_event_sink(None);
                let panicked = accept_thread.join().is_err();
                let reason = if panicked {
                    "pipe accept thread panicked before becoming ready"
                } else {
                    "pipe accept thread exited before becoming ready"
                };
                Err(io::Error::other(reason))
            }
        }
    }

    /// Stops accepting and disconnects every live connection.
    pub fn stop(&self) {
        self.stop.set();
        self.connections.disconnect_all();
    }

    /// Live pipe-connection count — drives `serve()`'s idle self-stop (ADR-0027).
    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Blocks until the accept and connection threads have exited.
    pub fn join(mut self: Arc<Self>) {
        if let Some(s) = Arc::get_mut(&mut self) {
            s.join_all_threads();
        } else {
            tracing::warn!("pipe accept join deferred until the final Server owner is dropped");
        }
    }

    fn join_all_threads(&mut self) {
        if let Some(thread) = self.accept_thread.take()
            && thread.join().is_err()
        {
            tracing::error!("pipe accept thread panicked");
        }
        // No accept can race this second snapshot. Every active reader is
        // interrupted before joining its connection thread.
        self.connections.disconnect_all();
        self.connections.join_all();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.set();
        self.connections.disconnect_all();
        self.join_all_threads();
    }
}

fn accept_loop(
    engine: Arc<Engine>,
    broadcaster: Arc<Broadcaster>,
    opts: ServerOptions,
    stop: &Event,
    active: Arc<std::sync::atomic::AtomicUsize>,
    connections: Arc<ConnectionRegistry>,
    ready_tx: mpsc::SyncSender<io::Result<()>>,
) {
    let mut ready_tx = Some(ready_tx);
    let security = if opts.authorized_sids.is_empty() {
        None
    } else {
        match crate::security::PipeSecurity::from_sddl(&crate::security::pipe_sddl(
            &opts.authorized_sids,
        )) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!(error = %e, "pipe SDDL conversion failed — not serving");
                // Refusing to serve wide-open beats serving wide-open. Surface
                // the failure synchronously so SCM never reports Running.
                if let Some(tx) = ready_tx.take()
                    && tx.send(Err(e)).is_err()
                {
                    tracing::debug!("pipe startup receiver was already gone");
                }
                return;
            }
        }
    };
    let mut listener = PipeListener::new(&opts.pipe_name, MAX_INSTANCES, security);
    loop {
        let accepted = listener.accept(stop, || {
            if let Some(tx) = ready_tx.take()
                && tx.send(Ok(())).is_err()
            {
                tracing::debug!("pipe startup receiver was already gone");
            }
        });
        match accepted {
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
                if let Err(e) = crate::lifecycle::stamp_last_use(&opts.data_dir) {
                    Counters::bump(&engine.metrics().counters.pipe_connections_rejected);
                    tracing::error!(
                        path = %crate::lifecycle::last_use_path(&opts.data_dir).display(),
                        error = %e,
                        "last_use publication failed — rejecting connection"
                    );
                    stream.disconnect();
                    continue;
                }
                let connection_engine = engine.clone();
                let broadcaster = broadcaster.clone();
                let faults = Faults::new(opts.debug_faults);
                let active = active.clone();
                let (cancel_tx, cancel_rx) = cancellation_channel();
                let connection_id = connections.register(stream.clone(), cancel_tx.clone());
                let connection_registry = connections.clone();
                match std::thread::Builder::new()
                    .name("fmf-pipe-conn".to_string())
                    .spawn(move || {
                        struct RegistryGuard {
                            registry: Arc<ConnectionRegistry>,
                            id: u64,
                        }
                        impl Drop for RegistryGuard {
                            fn drop(&mut self) {
                                self.registry.unregister(self.id);
                            }
                        }
                        let _registry_guard = RegistryGuard {
                            registry: connection_registry,
                            id: connection_id,
                        };
                        run_connection(
                            connection_engine,
                            broadcaster,
                            stream,
                            faults,
                            active,
                            cancel_tx,
                            cancel_rx,
                        );
                    }) {
                    Ok(thread) => connections.push_thread(thread),
                    Err(e) => {
                        connections.unregister(connection_id);
                        fmf_core::degrade!(
                            engine.metrics().counters.pipe_connections_rejected,
                            error = %e,
                            "pipe connection thread creation failed — rejecting connection"
                        );
                    }
                }
            }
            Err(e) => {
                if let Some(tx) = ready_tx.take() {
                    tracing::error!(error = %e, "initial pipe listen failed");
                    if tx.send(Err(e)).is_err() {
                        tracing::debug!("pipe startup receiver was already gone");
                    }
                    return;
                }
                // Typically ERROR_PIPE_BUSY at the instance cap — the
                // client was turned away by the OS; count, breathe, retry.
                Counters::bump(&engine.metrics().counters.pipe_connections_rejected);
                tracing::warn!(error = %e, "pipe accept failed — retrying");
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }
}

struct QueryGuard {
    connection: Arc<Connection>,
    request_id: u32,
    cancellation: Option<QueryCancellation>,
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.as_ref() {
            self.connection.finish_query(self.request_id, cancellation);
        }
    }
}

fn run_connection(
    engine: Arc<Engine>,
    broadcaster: Arc<Broadcaster>,
    stream: PipeStream,
    faults: Faults,
    active: Arc<std::sync::atomic::AtomicUsize>,
    cancel_tx: Sender<()>,
    cancel_rx: Receiver<()>,
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
    // At most one event queue per connection; Subscribe is idempotent.
    let subscription: Arc<Mutex<Option<Arc<crate::events::EventQueue>>>> =
        Arc::new(Mutex::new(None));

    let (tx, rx) = request_channel();
    let mut workers = Vec::new();
    for _ in 0..WORKERS_PER_CONNECTION {
        let rx = rx.clone();
        let conn = conn.clone();
        let writer = writer.clone();
        let broadcaster = broadcaster.clone();
        let subscription = subscription.clone();
        let stream = stream.clone();
        let worker_engine = engine.clone();
        let worker_cancel_tx = cancel_tx.clone();
        let worker_cancel_rx = cancel_rx.clone();
        match std::thread::Builder::new()
            .name("fmf-pipe-worker".to_string())
            .spawn(move || {
                loop {
                    let Some(request) = dequeue_request(&rx, &worker_cancel_rx) else {
                        return;
                    };
                    let header = request.header;
                    let _query_guard = QueryGuard {
                        connection: Arc::clone(&conn),
                        request_id: header.request_id,
                        cancellation: request.query_cancellation.clone(),
                    };
                    match conn.dispatch_with_query(
                        header.opcode,
                        header.request_id,
                        &request.payload,
                        request.query_cancellation.as_ref(),
                    ) {
                        Outcome::Reply(status, body) => {
                            let h = FrameHeader {
                                len: 0,
                                opcode: header.opcode,
                                flags: FLAG_RESPONSE,
                                request_id: header.request_id,
                                status,
                            };
                            if frame::write_frame(&mut *writer.lock(), h, &body).is_err() {
                                disconnect(&worker_cancel_tx, &stream);
                                return; // client went away; reader notices too
                            }
                        }
                        Outcome::Subscribe => {
                            let mut status = codes::OK;
                            {
                                let mut sub = subscription.lock();
                                if sub.is_none() {
                                    let q = broadcaster.subscribe();
                                    match spawn_event_writer(q.clone(), writer.clone()) {
                                        Ok(()) => *sub = Some(q),
                                        Err(e) => {
                                            broadcaster.unsubscribe(&q);
                                            fmf_core::degrade!(
                                                worker_engine
                                                    .metrics()
                                                    .counters
                                                    .pipe_events_dropped,
                                                error = %e,
                                                "pipe event-writer thread creation failed"
                                            );
                                            status = codes::IO;
                                        }
                                    }
                                }
                            }
                            let h = FrameHeader {
                                len: 0,
                                opcode: header.opcode,
                                flags: FLAG_RESPONSE,
                                request_id: header.request_id,
                                status,
                            };
                            if frame::write_frame(&mut *writer.lock(), h, &[]).is_err() {
                                disconnect(&worker_cancel_tx, &stream);
                                return;
                            }
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
                            if frame::write_frame(&mut *writer.lock(), h, &[]).is_err() {
                                disconnect(&worker_cancel_tx, &stream);
                                return;
                            }
                        }
                        Outcome::Drop => {
                            disconnect(&worker_cancel_tx, &stream);
                            return;
                        }
                    }
                }
            }) {
            Ok(worker) => workers.push(worker),
            Err(e) => {
                fmf_core::degrade!(
                    engine.metrics().counters.pipe_connections_rejected,
                    error = %e,
                    "pipe request-worker thread creation failed"
                );
            }
        }
    }
    if workers.is_empty() {
        conn.cancel_all_queries();
        disconnect(&cancel_tx, &stream);
        return;
    }

    // Reader: the only thread that touches the receive side.
    let mut reader = stream.clone();
    'reader: loop {
        match frame::read_frame_capped(&mut reader, |header| request_payload_cap(header.opcode)) {
            Ok((header, payload)) => {
                // Requests must not carry response/event flags.
                if header.flags != 0 {
                    Counters::bump(&engine.metrics().counters.pipe_malformed_frames);
                    tracing::warn!("malformed frame (flags on a request) — dropping connection");
                    stream.disconnect();
                    break;
                }

                // QueryCancel is a one-way control message and never enters
                // the work queue. Therefore a long-running query or saturated
                // queue cannot put cancellation behind its target.
                if header.opcode == messages::opcode::QUERY_CANCEL {
                    if !conn.hello_done.load(Ordering::Acquire) {
                        stream.disconnect();
                        break;
                    }
                    conn.cancel_query(header.request_id);
                    continue;
                }

                let query_cancellation = (header.opcode == messages::opcode::QUERY
                    && conn.hello_done.load(Ordering::Acquire))
                .then(|| conn.begin_query(header.request_id));
                let mut request = Request {
                    header,
                    payload,
                    query_cancellation,
                };

                // The reader never blocks on backpressure: otherwise a
                // subsequent QueryCancel would be unreadable. A newest Query
                // makes room by cancelling/dropping queued older work; a
                // non-query gets an explicit busy response.
                loop {
                    match tx.try_send(request) {
                        Ok(()) => break,
                        Err(TrySendError::Disconnected(_)) => break 'reader,
                        Err(TrySendError::Full(returned)) => {
                            request = returned;
                            if request.header.opcode != messages::opcode::QUERY {
                                let response = FrameHeader {
                                    len: 0,
                                    opcode: request.header.opcode,
                                    flags: FLAG_RESPONSE,
                                    request_id: request.header.request_id,
                                    status: codes::IO,
                                };
                                if frame::write_frame(
                                    &mut *writer.lock(),
                                    response,
                                    b"request queue busy",
                                )
                                .is_err()
                                {
                                    break 'reader;
                                }
                                break;
                            }

                            match rx.try_recv() {
                                Ok(dropped) => {
                                    if let Some(cancellation) = dropped.query_cancellation.as_ref()
                                    {
                                        cancellation.cancel();
                                        conn.finish_query(dropped.header.request_id, cancellation);
                                    }
                                    let response = FrameHeader {
                                        len: 0,
                                        opcode: dropped.header.opcode,
                                        flags: FLAG_RESPONSE,
                                        request_id: dropped.header.request_id,
                                        status: if dropped.header.opcode == messages::opcode::QUERY
                                        {
                                            codes::CANCELLED
                                        } else {
                                            codes::IO
                                        },
                                    };
                                    let detail: &[u8] = if response.status == codes::CANCELLED {
                                        b"query superseded"
                                    } else {
                                        b"request superseded"
                                    };
                                    if frame::write_frame(&mut *writer.lock(), response, detail)
                                        .is_err()
                                    {
                                        break 'reader;
                                    }
                                }
                                Err(TryRecvError::Empty) => {
                                    // A worker won the race between Full and
                                    // try_recv; retrying the nonblocking send
                                    // now succeeds without stalling the reader.
                                    std::thread::yield_now();
                                }
                                Err(TryRecvError::Disconnected) => break 'reader,
                            }
                        }
                    }
                }
            }
            Err(FrameError::TooLong { len, maximum }) => {
                Counters::bump(&engine.metrics().counters.pipe_malformed_frames);
                tracing::warn!(len, maximum, "oversized frame — dropping connection");
                stream.disconnect();
                break;
            }
            Err(FrameError::Io(_)) => break, // disconnect / shutdown
        }
    }

    conn.cancel_all_queries();
    cancel(&cancel_tx);
    drop(tx);
    for w in workers {
        if w.join().is_err() {
            tracing::error!("pipe request worker panicked");
        }
    }
    if let Some(q) = subscription.lock().take() {
        broadcaster.unsubscribe(&q); // closes the queue → event writer exits
    }
}

fn spawn_event_writer(
    q: Arc<crate::events::EventQueue>,
    writer: Arc<Mutex<PipeStream>>,
) -> io::Result<()> {
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
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::{
        REQUEST_QUEUE_CAP, Request, cancel, cancellation_channel, dequeue_request, request_channel,
        request_payload_cap,
    };
    use crossbeam_channel::TrySendError;
    use fmf_core::engine::QueryCancellation;
    use fmf_proto::frame::FrameHeader;
    use fmf_proto::messages::{self, opcode};

    fn request(request_id: u32) -> Request {
        Request {
            header: FrameHeader {
                len: 0,
                opcode: 0,
                flags: 0,
                request_id,
                status: 0,
            },
            payload: Vec::new(),
            query_cancellation: None,
        }
    }

    #[test]
    fn request_payload_caps_are_operation_specific_and_fail_closed() {
        for opcode in [
            opcode::SUBSCRIBE,
            opcode::UNSUBSCRIBE,
            opcode::LIST_VOLUMES,
            opcode::INDEX_STATUS,
            opcode::STATS,
            opcode::SERVICE_INFO,
            opcode::QUERY_CANCEL,
        ] {
            assert_eq!(request_payload_cap(opcode), 0);
        }
        assert_eq!(request_payload_cap(opcode::HELLO), 4);
        assert_eq!(
            request_payload_cap(opcode::INDEX_START),
            fmf_proto::limits::MAX_INDEX_START_PAYLOAD_LEN
        );
        assert_eq!(
            request_payload_cap(opcode::QUERY),
            messages::FmfQueryOptions::LEN as u32 + fmf_proto::limits::MAX_QUERY_BYTES
        );
        assert_eq!(
            request_payload_cap(opcode::RESULT_PAGE),
            messages::ResultPageReq::LEN as u32
        );
        assert_eq!(request_payload_cap(opcode::RESULT_FREE), 8);
        assert_eq!(
            request_payload_cap(u16::MAX),
            0,
            "unknown operations must not allocate caller-announced payloads"
        );
    }

    #[test]
    fn stopped_workers_cannot_grow_the_request_queue_past_the_cap() {
        let (tx, _rx) = request_channel();
        for request_id in 0..REQUEST_QUEUE_CAP {
            tx.try_send(request(request_id as u32))
                .expect("each slot through the documented cap is available");
        }
        assert!(matches!(
            tx.try_send(request(REQUEST_QUEUE_CAP as u32)),
            Err(TrySendError::Full(_))
        ));
        assert_eq!(
            tx.len(),
            REQUEST_QUEUE_CAP,
            "a stalled worker must not turn the request queue into an allocator"
        );
    }

    #[test]
    fn cancellation_prevents_workers_from_draining_queued_requests() {
        let (tx, rx) = request_channel();
        tx.send(request(1)).expect("queue request");
        let (cancel_tx, cancel_rx) = cancellation_channel();

        cancel(&cancel_tx);

        assert!(
            dequeue_request(&rx, &cancel_rx).is_none(),
            "cancel must win over already-queued work"
        );
        assert_eq!(rx.len(), 1, "cancelled work must remain undispatched");
    }

    #[test]
    fn queued_request_carries_pre_registered_query_cancellation() {
        let cancellation = QueryCancellation::new();
        let request = Request {
            header: FrameHeader {
                len: 0,
                opcode: opcode::QUERY,
                flags: 0,
                request_id: 7,
                status: 0,
            },
            payload: Vec::new(),
            query_cancellation: Some(cancellation.clone()),
        };
        cancellation.cancel();
        assert!(
            request
                .query_cancellation
                .as_ref()
                .is_some_and(QueryCancellation::is_cancelled)
        );
    }
}
