//! The serve core (shared by console `run` and the SCM entry) and the SCM
//! plumbing.
//!
//! Stop sources — Ctrl+C, `SERVICE_CONTROL_STOP`, PRESHUTDOWN — all funnel
//! into one shared `AtomicBool`; teardown is always stop-accepting →
//! flush → shutdown (docs/ARCHITECTURE.md, ADR-0016).

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fmf_core::engine::VolumeState;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::{config, host, lifecycle, security, server};

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
    /// Require a valid config with at least one authorized SID. Always true for
    /// the installed SCM service; console/test mode explicitly opts out.
    pub require_authorization: bool,
}

/// Exit code reported when the writer lock never came free.
///
/// Visible in the event log, but a clean `SERVICE_STOPPED` so the SCM does not
/// crash-loop us against the lock holder (docs/ARCHITECTURE.md §single-writer exclusion).
pub const EXIT_LOCKED: u32 = fmf_proto::codes::LOCKED as u32;

/// Brings the engine + pipe server up, parks until `stop`, tears down.
///
/// # Errors
/// Returns a shared contract status on startup failure: [`EXIT_LOCKED`] when
/// the writer lock never freed, or `FMF_E_IO` for other bring-up failures.
pub fn serve(
    opts: &ServeOptions,
    stop: &Arc<AtomicBool>,
    on_ready: impl FnOnce() -> Result<(), u32>,
) -> Result<(), u32> {
    let config_path = opts.data_dir.join("service.json");
    let cfg = load_service_config(&config_path, opts.require_authorization)?;
    // On-demand idle self-stop (ADR-0027); 0 = disabled (legacy resident).
    let idle_stop = Duration::from_secs(cfg.idle_stop_secs);

    let engine = match host::create_engine_with_retry(opts.data_dir.join("index"), stop, 10) {
        Ok(e) => e,
        Err(fmf_core::engine::EngineCreateError::Locked(_)) => return Err(EXIT_LOCKED),
        Err(e) => {
            tracing::error!(error = %e, "engine create failed");
            return Err(fmf_proto::codes::IO as u32);
        }
    };

    if !opts.no_index {
        let volumes = fmf_core::engine::Engine::list_ntfs_volumes();
        tracing::info!(?volumes, "indexing");
        if let Err(error) = engine.index_start(&volumes) {
            // The OS volume set can change between enumeration and start.
            // Keep serving the remaining engine surface; a later explicit
            // IndexStart re-enumerates and can succeed.
            tracing::warn!(%error, "startup volume set changed before indexing");
        }
    }

    let srv = match server::Server::start(
        engine.clone(),
        server::ServerOptions {
            pipe_name: opts.pipe_name.clone(),
            debug_faults: opts.debug_faults,
            authorized_sids: cfg.authorized_sids.clone(),
            data_dir: opts.data_dir.clone(),
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "pipe server start failed");
            return Err(fmf_proto::codes::IO as u32);
        }
    };
    // Periodic flush: dirty volumes only (Engine::flush's contract).
    let flush_engine = engine.clone();
    let flush_stop = stop.clone();
    let interval = Duration::from_secs(cfg.flush_interval_secs);
    let flusher = match std::thread::Builder::new()
        .name("fmf-periodic-flush".into())
        .spawn(move || {
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
        }) {
        Ok(thread) => thread,
        Err(e) => {
            tracing::error!(error = %e, "periodic flush thread creation failed");
            srv.stop();
            srv.join();
            engine.set_event_sink(None);
            engine.shutdown();
            return Err(fmf_proto::codes::IO as u32);
        }
    };

    if let Err(code) = on_ready() {
        tracing::error!("ready-state publication failed");
        stop.store(true, Ordering::Relaxed);
        srv.stop();
        srv.join();
        if flusher.join().is_err() {
            tracing::error!("periodic flush thread panicked during failed startup cleanup");
        }
        engine.set_event_sink(None);
        engine.shutdown();
        return Err(code);
    }
    tracing::info!(pipe = %opts.pipe_name, "serving");

    // Park until stopped (SCM Stop / Ctrl+C) or, when idle-stop is enabled,
    // until the service has sat with no live connection past its timeout
    // (ADR-0027). The idle clock starts only once a client has connected and
    // dropped (`seen_client`), and a self-stop is held off while an initial
    // scan is still running.
    let mut seen_client = false;
    let mut idle_since = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
        if idle_stop.is_zero() {
            continue;
        }
        if srv.active_connections() > 0 {
            seen_client = true;
            idle_since = Instant::now();
            continue;
        }
        if !seen_client || idle_since.elapsed() < idle_stop {
            continue;
        }
        // Idle past the timeout with no client. Stop unless an initial scan is
        // still in flight (the rare client-less bring-up); status() is consulted
        // only here, at most once per idle window.
        let indexing = engine
            .status()
            .iter()
            .any(|(_, state, _)| *state == VolumeState::Scanning);
        if lifecycle::idle_should_stop(seen_client, 0, indexing, idle_since.elapsed(), idle_stop) {
            tracing::info!(
                idle_secs = idle_stop.as_secs(),
                "idle — self-stopping (on-demand)"
            );
            // Signal stop so the periodic-flush thread exits and the teardown
            // below (flusher.join) doesn't block.
            stop.store(true, Ordering::Relaxed);
            break;
        }
        idle_since = Instant::now(); // a scan held us off; re-check next window
    }
    tracing::info!("stopping — flushing snapshots");
    let last_use_error = lifecycle::stamp_last_use(&opts.data_dir).err();
    if let Some(e) = &last_use_error {
        tracing::error!(
            path = %lifecycle::last_use_path(&opts.data_dir).display(),
            error = %e,
            "last_use publication failed during shutdown"
        );
    }
    srv.stop();
    srv.join();
    if flusher.join().is_err() {
        tracing::error!("periodic flush thread panicked during shutdown");
    }
    engine.flush();
    engine.set_event_sink(None);
    engine.shutdown();
    if last_use_error.is_some() {
        Err(fmf_proto::codes::IO as u32)
    } else {
        Ok(())
    }
}

fn load_service_config(
    path: &std::path::Path,
    require_authorization: bool,
) -> Result<config::ServiceConfig, u32> {
    if !require_authorization {
        return Ok(config::ServiceConfig::load_or_default(path));
    }
    match config::ServiceConfig::try_load(path) {
        Ok(config) if !config.authorized_sids.is_empty() => Ok(config),
        Ok(_) => {
            tracing::error!("installed service config has no authorized SID — refusing to serve");
            Err(fmf_proto::codes::IO as u32)
        }
        Err(e) => {
            tracing::error!(error = %e, "installed service config unreadable — refusing to serve");
            Err(fmf_proto::codes::IO as u32)
        }
    }
}

// ── SCM entry ───────────────────────────────────────────────────────────

define_windows_service!(ffi_service_main, service_main);

fn service_status(state: ServiceState, exit: ServiceExitCode) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::PRESHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: exit,
        checkpoint: u32::from(state == ServiceState::StartPending),
        wait_hint: if state == ServiceState::StartPending {
            Duration::from_secs(30)
        } else {
            Duration::ZERO
        },
        process_id: None,
    }
}

/// Called by the SCM dispatcher on the service thread.
fn service_main(_args: Vec<OsString>) {
    let stop = Arc::new(AtomicBool::new(false));
    let handler_stop = stop.clone();
    let status_handle =
        match service_control_handler::register(SERVICE_NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Preshutdown => {
                handler_stop.store(true, Ordering::Relaxed);
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

    if let Err(e) = status_handle.set_service_status(service_status(
        ServiceState::StartPending,
        ServiceExitCode::Win32(0),
    )) {
        eprintln!("fmf-service: SCM StartPending status failed: {e}");
        return;
    }

    let data_dir = match config::default_data_dir() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("fmf-service: ProgramData resolution failed: {e}");
            if let Err(status_error) = status_handle.set_service_status(service_status(
                ServiceState::Stopped,
                ServiceExitCode::ServiceSpecific(fmf_proto::codes::IO as u32),
            )) {
                eprintln!("fmf-service: SCM Stopped status failed: {status_error}");
            }
            return;
        }
    };
    if let Err(e) = security::validate_installed_data_paths(&data_dir) {
        eprintln!("fmf-service: installed data-path validation failed: {e}");
        if let Err(status_error) = status_handle.set_service_status(service_status(
            ServiceState::Stopped,
            ServiceExitCode::ServiceSpecific(fmf_proto::codes::IO as u32),
        )) {
            eprintln!("fmf-service: SCM Stopped status failed: {status_error}");
        }
        return;
    }
    let config_path = data_dir.join("service.json");
    // The required load inside `serve` records any error after this bootstrap
    // logger exists; this pre-read only selects the filter used to create it.
    let (cfg, _) = config::ServiceConfig::load_or_default_with_error(&config_path);
    fmf_core::diag::init_diag(
        Some(&data_dir.join("logs")),
        &cfg.log_level,
        fmf_core::diag::SERVICE_MAX_LOG_FILES,
    );

    let exit = match serve(
        &ServeOptions {
            data_dir,
            pipe_name: fmf_proto::PIPE_NAME.to_string(),
            debug_faults: false,
            no_index: false,
            require_authorization: true,
        },
        &stop,
        || {
            status_handle
                .set_service_status(service_status(
                    ServiceState::Running,
                    ServiceExitCode::Win32(0),
                ))
                .map_err(|e| {
                    tracing::error!(error = %e, "SCM Running status update failed");
                    fmf_proto::codes::IO as u32
                })
        },
    ) {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(code) => ServiceExitCode::ServiceSpecific(code),
    };
    if let Err(e) = status_handle.set_service_status(service_status(ServiceState::Stopped, exit)) {
        tracing::error!(error = %e, "SCM Stopped status update failed");
    }
}

/// Blocks for the service lifetime; fails fast when not launched by the SCM.
///
/// # Errors
/// Returns the `windows_service` error when the SCM dispatcher cannot start
/// (e.g. the process was launched directly rather than by the SCM).
pub fn dispatch() -> windows_service::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_service_config_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("service.json");

        assert_eq!(
            load_service_config(&path, true).expect_err("missing config must fail"),
            fmf_proto::codes::IO as u32
        );
        std::fs::write(&path, b"{").expect("write corrupt config");
        assert_eq!(
            load_service_config(&path, true).expect_err("corrupt config must fail"),
            fmf_proto::codes::IO as u32
        );
        config::ServiceConfig::default()
            .save(&path)
            .expect("write empty allowlist");
        assert_eq!(
            load_service_config(&path, true).expect_err("empty allowlist must fail"),
            fmf_proto::codes::IO as u32
        );

        let mut valid = config::ServiceConfig::default();
        valid.authorized_sids.push("S-1-5-21-1-2-3-1001".into());
        valid.save(&path).expect("write authorized config");
        assert_eq!(
            load_service_config(&path, true)
                .expect("authorized config")
                .authorized_sids,
            valid.authorized_sids
        );
    }

    #[test]
    fn scm_status_is_pending_until_the_pipe_is_ready() {
        let pending = service_status(ServiceState::StartPending, ServiceExitCode::Win32(0));
        assert!(pending.controls_accepted.is_empty());
        assert_eq!(pending.checkpoint, 1);
        assert_eq!(pending.wait_hint, Duration::from_secs(30));

        let running = service_status(ServiceState::Running, ServiceExitCode::Win32(0));
        assert!(
            running
                .controls_accepted
                .contains(ServiceControlAccept::STOP)
        );
        assert!(
            running
                .controls_accepted
                .contains(ServiceControlAccept::PRESHUTDOWN)
        );
        assert_eq!(running.checkpoint, 0);
        assert_eq!(running.wait_hint, Duration::ZERO);

        let stopped = service_status(ServiceState::Stopped, ServiceExitCode::Win32(0));
        assert!(stopped.controls_accepted.is_empty());
        assert_eq!(stopped.wait_hint, Duration::ZERO);
    }
}
