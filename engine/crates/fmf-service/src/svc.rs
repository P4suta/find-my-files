//! The serve core (shared by console `run` and the SCM entry) and the SCM
//! plumbing.
//!
//! Stop sources — Ctrl+C, `SERVICE_CONTROL_STOP`, PRESHUTDOWN — all funnel
//! into one (`AtomicBool`, Event) pair; teardown is always stop-accepting →
//! flush → shutdown (docs/ARCHITECTURE.md, ADR-0016).

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::pipe::Event;
use crate::{config, host, server};

// The SCM name is contract surface (the app's in-app service setup needs
// it too — ADR-0018 radiation).
pub use fmf_proto::SERVICE_NAME;

/// Knobs for [`serve`]: where data lives, which pipe to bind, and dev toggles.
pub struct ServeOptions {
    /// Machine-wide data root (`%ProgramData%\find-my-files`); holds
    /// `service.json`, the `index` snapshot dir, and `logs`.
    pub data_dir: std::path::PathBuf,
    /// Named-pipe address the server listens on for UI clients.
    pub pipe_name: String,
    /// Enable the `--debug-faults` query hooks (`!!panic` / `!!drop` / `!!lag`);
    /// always off for an installed service.
    pub debug_faults: bool,
    /// Skip the initial volume index on startup (serve the existing snapshot
    /// only); used for fast bring-up in dev.
    pub no_index: bool,
}

/// Exit code reported when the writer lock never came free.
///
/// Visible in the event log, but a clean `SERVICE_STOPPED` so the SCM does not
/// crash-loop us against the lock holder (docs/ARCHITECTURE.md §single-writer exclusion).
pub const EXIT_LOCKED: u32 = 7;

/// Brings the engine + pipe server up, parks until `stop`, tears down.
///
/// # Errors
/// Returns a process exit code on startup failure: [`EXIT_LOCKED`] when the
/// writer lock never freed, or `1` for any other engine/pipe bring-up error.
pub fn serve(
    opts: &ServeOptions,
    stop: &Arc<AtomicBool>,
    stop_event: &Arc<Event>,
) -> Result<(), u32> {
    let cfg = config::ServiceConfig::load(&opts.data_dir.join("service.json"));

    let engine = match host::create_engine_with_retry(opts.data_dir.join("index"), stop, 10) {
        Ok(e) => e,
        Err(fmf_core::engine::EngineCreateError::Locked(_)) => return Err(EXIT_LOCKED),
        Err(e) => {
            tracing::error!(error = %e, "engine create failed");
            return Err(1);
        }
    };

    if !opts.no_index {
        let volumes = if cfg.volumes.is_empty() {
            fmf_core::engine::Engine::list_ntfs_volumes()
        } else {
            cfg.volumes
        };
        tracing::info!(?volumes, "indexing");
        engine.index_start(&volumes);
    }

    let srv = match server::Server::start(
        engine.clone(),
        server::ServerOptions {
            pipe_name: opts.pipe_name.clone(),
            debug_faults: opts.debug_faults,
            authorized_sids: cfg.authorized_sids.clone(),
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "pipe server start failed");
            return Err(1);
        }
    };
    tracing::info!(pipe = %opts.pipe_name, "serving");

    // Periodic flush: dirty volumes only (Engine::flush's contract).
    let flush_engine = engine.clone();
    let flush_stop = stop.clone();
    let interval = Duration::from_secs(cfg.flush_interval_secs.max(10));
    let flusher = std::thread::spawn(move || {
        loop {
            let mut waited = Duration::ZERO;
            while waited < interval {
                if flush_stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
                waited += Duration::from_millis(500);
            }
            let saved = flush_engine.flush();
            if saved > 0 {
                tracing::info!(saved, "periodic flush");
            }
        }
    });

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = stop_event; // accept-loop wakeups go through Server::stop below

    tracing::info!("stopping — flushing snapshots");
    srv.stop();
    let _ = flusher.join();
    engine.flush();
    engine.set_event_sink(None);
    engine.shutdown();
    Ok(())
}

// ── SCM entry ───────────────────────────────────────────────────────────

static SCM_STOP: AtomicBool = AtomicBool::new(false);
static SCM_EVENT: parking_lot::Mutex<Option<Arc<Event>>> = parking_lot::Mutex::new(None);

define_windows_service!(ffi_service_main, service_main);

/// Called by the SCM dispatcher on the service thread.
fn service_main(_args: Vec<OsString>) {
    let data_dir = config::default_data_dir();
    let cfg = config::ServiceConfig::load(&data_dir.join("service.json"));
    fmf_core::diag::init_diag(Some(&data_dir.join("logs")), &cfg.log_level);

    let status_handle =
        match service_control_handler::register(SERVICE_NAME, |control| match control {
            ServiceControl::Stop | ServiceControl::Preshutdown => {
                SCM_STOP.store(true, Ordering::Relaxed);
                if let Some(ev) = SCM_EVENT.lock().as_ref() {
                    ev.set();
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "SCM handler registration failed");
                return;
            }
        };

    let running = |state: ServiceState, exit: ServiceExitCode| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::PRESHUTDOWN,
        exit_code: exit,
        checkpoint: 0,
        wait_hint: Duration::from_secs(30),
        process_id: None,
    };
    let _ =
        status_handle.set_service_status(running(ServiceState::Running, ServiceExitCode::Win32(0)));

    let stop = Arc::new(AtomicBool::new(false));
    let stop_event = Arc::new(Event::new().expect("stop event"));
    *SCM_EVENT.lock() = Some(stop_event.clone());
    // Bridge the SCM-global flag into the local one.
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !SCM_STOP.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
            stop.store(true, Ordering::Relaxed);
        });
    }

    let exit = match serve(
        &ServeOptions {
            data_dir,
            pipe_name: fmf_proto::PIPE_NAME.to_string(),
            debug_faults: false,
            no_index: false,
        },
        &stop,
        &stop_event,
    ) {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(code) => ServiceExitCode::ServiceSpecific(code),
    };
    let _ = status_handle.set_service_status(running(ServiceState::Stopped, exit));
}

/// Blocks for the service lifetime; fails fast when not launched by the SCM.
///
/// # Errors
/// Returns the `windows_service` error when the SCM dispatcher cannot start
/// (e.g. the process was launched directly rather than by the SCM).
pub fn dispatch() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}
