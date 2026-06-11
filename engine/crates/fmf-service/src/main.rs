//! fmf-service console entry (`run`). SCM integration (install/uninstall/
//! start/stop/status, SDDL hardening) lands with the hardening phase; the
//! console mode is the dev loop and the integration-test harness either way.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use fmf_service::pipe::Event;
use fmf_service::{config, host, server};

#[derive(Parser)]
#[command(name = "fmf-service", about = "find-my-files engine service")]
enum Cli {
    /// Run in the foreground (console mode; Ctrl+C = flush + graceful stop).
    Run {
        /// Pipe path override (tests use a unique name; the default is the
        /// contract name).
        #[arg(long, default_value = fmf_proto::PIPE_NAME)]
        pipe_name: String,
        /// Index/log/config root override (default: %ProgramData%\find-my-files).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// Enable !!panic / !!drop / !!lag fault injection (never on by default).
        #[arg(long)]
        debug_faults: bool,
        /// Skip the startup index_start (unelevated debugging of the pipe
        /// surface; volumes can still be started over the pipe).
        #[arg(long)]
        no_index: bool,
    },
}

static STOP: AtomicBool = AtomicBool::new(false);
static STOP_EVENT: parking_lot::Mutex<Option<Arc<Event>>> = parking_lot::Mutex::new(None);

fn main() -> std::process::ExitCode {
    match Cli::parse() {
        Cli::Run {
            pipe_name,
            data_dir,
            debug_faults,
            no_index,
        } => run(pipe_name, data_dir, debug_faults, no_index),
    }
}

fn run(
    pipe_name: String,
    data_dir: Option<std::path::PathBuf>,
    debug_faults: bool,
    no_index: bool,
) -> std::process::ExitCode {
    let data_dir = data_dir.unwrap_or_else(config::default_data_dir);
    let cfg = config::ServiceConfig::load(&data_dir.join("service.json"));
    fmf_core::diag::init_logging(Some(&data_dir.join("logs")), &cfg.log_level);
    fmf_core::diag::install_panic_hook();
    install_ctrl_c();

    let engine = match host::create_engine_with_retry(data_dir.join("index"), &STOP, 10) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(error = %e, "engine create failed — exiting");
            eprintln!("fmf-service: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    if !no_index {
        let volumes = if cfg.volumes.is_empty() {
            fmf_core::engine::Engine::list_ntfs_volumes()
        } else {
            cfg.volumes.clone()
        };
        tracing::info!(?volumes, "indexing");
        engine.index_start(&volumes);
    }

    let srv = match server::Server::start(
        engine.clone(),
        server::ServerOptions {
            pipe_name: pipe_name.clone(),
            debug_faults,
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "pipe server start failed");
            eprintln!("fmf-service: pipe server: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("fmf-service: serving on {pipe_name} (Ctrl+C to stop)");

    // Periodic flush: dirty volumes only (Engine::flush's contract).
    let flush_engine = engine.clone();
    let interval = std::time::Duration::from_secs(cfg.flush_interval_secs.max(10));
    let flusher = std::thread::spawn(move || {
        loop {
            let mut waited = std::time::Duration::ZERO;
            while waited < interval {
                if STOP.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                waited += std::time::Duration::from_millis(500);
            }
            let saved = flush_engine.flush();
            if saved > 0 {
                tracing::info!(saved, "periodic flush");
            }
        }
    });

    // Park until Ctrl+C.
    {
        let ev = Arc::new(Event::new().expect("stop event"));
        *STOP_EVENT.lock() = Some(ev.clone());
        while !STOP.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    println!("fmf-service: stopping — flushing snapshots");
    srv.stop();
    let _ = flusher.join();
    engine.flush();
    engine.set_event_sink(None);
    engine.shutdown();
    std::process::ExitCode::SUCCESS
}

fn install_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
        STOP.store(true, Ordering::Relaxed);
        if let Some(ev) = STOP_EVENT.lock().as_ref() {
            ev.set();
        }
        1 // handled — give main time to flush
    }
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}
