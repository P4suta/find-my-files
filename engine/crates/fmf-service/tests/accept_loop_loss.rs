//! Unelevated regression: a fatal accept-loop exit must take the *service*
//! down instead of leaving it Running behind a pipe nobody can reach.
//!
//! Failing closed on a broken client-verification API (docs/SECURITY.md layer
//! 4) is correct — the dangerous alternative is admitting clients no one could
//! check. What must not happen is the silent half: the listener disappears, the
//! SCM keeps reporting Running, clients reconnect forever, and `fmf-service
//! start` is a no-op against a "running" service. With `idle_stop_secs = 0`, or
//! before any client has ever connected, nothing else could ever end that state.
//!
//! The verifier is injected (`ServerOptions::client_verifier` /
//! `ServeOptions::client_verifier`) because the real `security::verify_client`
//! only returns `Err` against live Windows tokens — an OS API failure, or a
//! client that vanished between `ConnectNamedPipe` and
//! `ImpersonateNamedPipeClient`. Injection makes that deterministic and keeps
//! the whole regression unelevated: both tests drive the real accept loop, the
//! real named pipe, and (below) the real `serve()` park loop.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use fmf_core::engine::{Engine, EngineConfig};
use fmf_core::index::testutil::TestDir;
use fmf_service::pipe::PipeStream;
use fmf_service::server::{Server, ServerOptions};
use fmf_service::svc::{self, ServeOptions};

/// Generous enough for a loaded machine; every wait below is a hard failure, so
/// none of them may be "flaky-tuned" down.
const DEADLINE: Duration = Duration::from_secs(30);

fn unique_name(tag: &str) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        r"\\.\pipe\fmf-test-{}-{}-{}",
        tag,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

static SERVER_VERIFY_CALLS: AtomicUsize = AtomicUsize::new(0);
static SERVE_VERIFY_CALLS: AtomicUsize = AtomicUsize::new(0);

fn server_broken_verifier(_pipe: &PipeStream, _authorized: &[String]) -> io::Result<bool> {
    SERVER_VERIFY_CALLS.fetch_add(1, Ordering::Release);
    Err(io::Error::other("client token verification API failed"))
}

fn serve_broken_verifier(_pipe: &PipeStream, _authorized: &[String]) -> io::Result<bool> {
    SERVE_VERIFY_CALLS.fetch_add(1, Ordering::Release);
    Err(io::Error::other("client token verification API failed"))
}

/// The pipe-server half: failing closed is fine, failing *silently* is not —
/// the lost listener has to become observable state.
#[test]
fn a_broken_client_verifier_makes_the_lost_listener_observable() {
    let dir = TestDir::new();
    let engine = Engine::new(EngineConfig {
        index_dir: dir.path().to_path_buf(),
    })
    .expect("engine");
    let pipe_name = unique_name("accept-loss");
    let server = Server::start(
        engine.clone(),
        ServerOptions {
            pipe_name: pipe_name.clone(),
            debug_faults: false,
            authorized_sids: Vec::new(),
            data_root: None,
            client_verifier: server_broken_verifier,
        },
    )
    .expect("server start");
    assert!(
        server.is_accepting(),
        "a freshly started server must report a live listener"
    );

    drop(PipeStream::connect(&pipe_name).expect("client reaches the listener"));

    let began = Instant::now();
    while server.is_accepting() {
        assert!(
            began.elapsed() < DEADLINE,
            "the accept loop exited on the verification error but never reported it — \
             serve() cannot notice a listener it is not told about"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        SERVER_VERIFY_CALLS.load(Ordering::Acquire) >= 1,
        "the connection must have gone through client verification"
    );
    assert_eq!(
        server.active_connections(),
        0,
        "a connection rejected before its thread starts must never count as a live client — \
         this is why the idle self-stop cannot rescue a service in this state"
    );
    engine.set_event_sink(None);
}

/// The service half: `serve()` must return (flushing on the way out) instead of
/// parking forever with a dead accept loop. `idle_stop_secs = 0` is the
/// configuration that used to be permanently unrecoverable.
#[test]
fn serve_stops_when_the_accept_loop_dies_even_with_idle_stop_disabled() {
    let data_dir = TestDir::new();
    std::fs::write(
        data_dir.join("service.json"),
        br#"{ "idle_stop_secs": 0, "flush_interval_secs": 10 }"#,
    )
    .expect("write service config");
    let pipe_name = unique_name("serve-accept-loss");

    // The only stop signal the SCM/Ctrl+C would ever raise. The test never sets
    // it: whatever ends `serve()` below has to come from inside.
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let serve_stop = stop.clone();
    let serve_pipe = pipe_name.clone();
    let serve_data_dir = data_dir.path().to_path_buf();
    let serve_thread = std::thread::Builder::new()
        .name("fmf-serve-under-test".to_string())
        .spawn(move || {
            let outcome = svc::serve(
                &ServeOptions {
                    data_dir: serve_data_dir,
                    data_root: None,
                    pipe_name: serve_pipe,
                    debug_faults: false,
                    no_index: true,
                    require_authorization: false,
                    client_verifier: serve_broken_verifier,
                },
                &serve_stop,
                || {
                    ready_tx.send(()).expect("publish serve readiness");
                    Ok(())
                },
            );
            done_tx.send(outcome).expect("publish serve outcome");
        })
        .expect("spawn serve");

    ready_rx
        .recv_timeout(DEADLINE)
        .expect("serve reached its ready state");
    drop(PipeStream::connect(&pipe_name).expect("client reaches the service pipe"));

    let outcome = done_rx.recv_timeout(DEADLINE).expect(
        "serve must return once its accept loop is gone: a service with no listener that keeps \
         reporting Running is unrecoverable — the UI reconnects forever and `start` is a no-op",
    );
    assert_eq!(
        outcome,
        Err(fmf_proto::codes::IO as u32),
        "losing the listener is a failure exit, not a clean stop"
    );
    assert!(
        stop.load(Ordering::Relaxed),
        "serve must raise the shared stop flag itself so the periodic-flush thread also winds down"
    );
    assert!(
        SERVE_VERIFY_CALLS.load(Ordering::Acquire) >= 1,
        "the connection must have gone through client verification"
    );
    assert!(
        PipeStream::connect(&pipe_name).is_err(),
        "no pipe may answer after serve returns"
    );
    serve_thread.join().expect("serve thread did not panic");
}
