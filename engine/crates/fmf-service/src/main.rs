//! fmf-service entry: console `run` (dev loop / test harness), the hidden
//! SCM entry, and the lifecycle subcommands. `install` is a subcommand and
//! not an sc.exe one-liner because it must do four things atomically:
//! capture the installing user's SID into service.json, harden the data-dir
//! DACLs, register with delayed start + crash recovery, and set the
//! preshutdown/privilege configs (ADR-0017).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use fmf_core::diag::error_chain;
use fmf_service::pipe::{Event, PipeStream};
use fmf_service::svc::{EXIT_LOCKED, SERVICE_NAME, ServeOptions};
use fmf_service::{config, security, svc};

#[derive(Parser)]
#[command(name = "fmf-service", about = "find-my-files engine service")]
enum Cli {
    /// Run in the foreground (console mode; Ctrl+C = flush + graceful stop).
    Run {
        /// Pipe path override (tests use a unique name).
        #[arg(long, default_value = fmf_proto::PIPE_NAME)]
        pipe_name: String,
        /// Index/log/config root override (default: %ProgramData%\find-my-files).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// Enable !!panic / !!drop / !!lag fault injection (never on by default).
        #[arg(long)]
        debug_faults: bool,
        /// Skip the startup index_start (unelevated pipe-surface debugging).
        #[arg(long)]
        no_index: bool,
    },
    /// Register the Windows service (elevated; captures your SID, hardens
    /// the data dir, sets delayed start + crash recovery + preshutdown).
    Install,
    /// Deregister the service. Data (index snapshots = every file name,
    /// logs, service.json) is kept unless --purge-data.
    Uninstall {
        #[arg(long)]
        purge_data: bool,
    },
    /// Start the installed service.
    Start,
    /// Stop the installed service.
    Stop,
    /// SCM state + a live pipe handshake ping.
    Status,
    /// (internal) SCM entry point — launched by the service controller.
    #[command(hide = true)]
    ServiceEntry,
}

static STOP: AtomicBool = AtomicBool::new(false);
static STOP_EVENT: parking_lot::Mutex<Option<Arc<Event>>> = parking_lot::Mutex::new(None);

fn main() -> std::process::ExitCode {
    let ok = match Cli::parse() {
        Cli::Run {
            pipe_name,
            data_dir,
            debug_faults,
            no_index,
        } => return run_console(pipe_name, data_dir, debug_faults, no_index),
        Cli::ServiceEntry => svc::dispatch().is_ok(),
        Cli::Install => report(install()),
        Cli::Uninstall { purge_data } => report(uninstall(purge_data)),
        Cli::Start => report(start_service()),
        Cli::Stop => report(stop_service()),
        Cli::Status => report(status()),
    };
    if ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

fn report(r: Result<(), String>) -> bool {
    match r {
        Ok(()) => true,
        Err(e) => {
            eprintln!("fmf-service: {e}");
            false
        }
    }
}

fn run_console(
    pipe_name: String,
    data_dir: Option<std::path::PathBuf>,
    debug_faults: bool,
    no_index: bool,
) -> std::process::ExitCode {
    let data_dir = data_dir.unwrap_or_else(config::default_data_dir);
    let cfg = config::ServiceConfig::load(&data_dir.join("service.json"));
    fmf_core::diag::init_diag(Some(&data_dir.join("logs")), &cfg.log_level);
    install_ctrl_c();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_event = Arc::new(Event::new().expect("stop event"));
    *STOP_EVENT.lock() = Some(stop_event.clone());
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !STOP.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
            stop.store(true, Ordering::Relaxed);
        });
    }

    println!("fmf-service: serving on {pipe_name} (Ctrl+C to stop)");
    match svc::serve(
        &ServeOptions {
            data_dir,
            pipe_name,
            debug_faults,
            no_index,
        },
        &stop,
        &stop_event,
    ) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(code) => {
            if code == EXIT_LOCKED {
                eprintln!(
                    "fmf-service: index dir is locked by another engine \
                     (an in-proc UI? close it or `just service-stop` the other instance)"
                );
            }
            std::process::ExitCode::FAILURE
        }
    }
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

// ── Lifecycle subcommands ───────────────────────────────────────────────

fn install() -> Result<(), String> {
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceInfo, ServiceStartType, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    // 1. Capture the installing user — the one identity allowed on the pipe.
    let sid = security::current_user_sid().map_err(|e| format!("SID capture: {e}"))?;

    // 2. Persist it (and create the data tree) before the SCM knows about us.
    let data_dir = config::default_data_dir();
    for sub in ["index", "logs"] {
        std::fs::create_dir_all(data_dir.join(sub)).map_err(|e| format!("data dir: {e}"))?;
    }
    let cfg_path = data_dir.join("service.json");
    let mut cfg = config::ServiceConfig::load(&cfg_path);
    if !cfg.authorized_sids.contains(&sid) {
        cfg.authorized_sids.push(sid.clone());
    }
    cfg.save(&cfg_path)
        .map_err(|e| format!("service.json: {e}"))?;

    // 3. Harden the tree: snapshots are machine-wide file listings
    //    (SECURITY.md 脅威7); logs keep user read for the F12 copy path.
    security::set_dir_dacl(&data_dir, &security::data_dir_sddl())
        .map_err(|e| format!("data dir DACL: {e}"))?;
    security::set_dir_dacl(&data_dir.join("logs"), &security::logs_dir_sddl(&sid))
        .map_err(|e| format!("logs DACL: {e}"))?;

    // 4. Register: LocalSystem, delayed auto start, restart-on-crash.
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| format!("SCM open (elevated?): {}", error_chain(&e)))?;
    let service = match manager.create_service(
        &ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: "find-my-files engine".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: std::env::current_exe().map_err(|e| e.to_string())?,
            launch_arguments: vec!["service-entry".into()],
            dependencies: vec![],
            account_name: None, // LocalSystem
            account_password: None,
        },
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
    ) {
        Ok(s) => s,
        // ERROR_SERVICE_EXISTS(1073): install is an idempotent ritual —
        // steps 1–3 already refreshed the SID/config/DACLs, so refresh the
        // registration's config too instead of failing with a cryptic
        // wrapper error (the original sin: "IO error in winapi call").
        Err(e) if raw_os_error(&e) == Some(1073) => {
            println!("'{SERVICE_NAME}' is already installed — refreshing its configuration");
            manager
                .open_service(
                    SERVICE_NAME,
                    ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
                )
                .map_err(|e| format!("open existing service: {}", error_chain(&e)))?
        }
        Err(e) => return Err(format!("create_service: {}", error_chain(&e))),
    };
    service
        .set_delayed_auto_start(true)
        .map_err(|e| format!("delayed start: {e}"))?;
    service
        .update_failure_actions(ServiceFailureActions {
            reset_period: windows_service::service::ServiceFailureResetPeriod::After(
                Duration::from_secs(86_400),
            ),
            reboot_msg: None,
            command: None,
            actions: Some(vec![
                ServiceAction {
                    action_type: ServiceActionType::Restart,
                    delay: Duration::from_secs(10),
                };
                3
            ]),
        })
        .map_err(|e| format!("failure actions: {e}"))?;

    // 5. Raw config2: strip privileges, stretch the preshutdown window
    //    (modern default is only 10s — docs/RESEARCH.md).
    set_required_privileges(&["SeChangeNotifyPrivilege"])?;
    set_preshutdown_timeout(Duration::from_secs(180))?;

    println!("installed '{SERVICE_NAME}' (LocalSystem, delayed auto start)");
    println!("authorized SID: {sid}");
    println!("start it with: fmf-service start  (or `just service-start`)");
    Ok(())
}

fn uninstall(purge_data: bool) -> Result<(), String> {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("SCM open: {e}"))?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )
        .map_err(|e| format!("open service: {e}"))?;
    if service
        .query_status()
        .map_err(|e| e.to_string())?
        .current_state
        != ServiceState::Stopped
    {
        let _ = service.stop();
        for _ in 0..50 {
            if service
                .query_status()
                .map_err(|e| e.to_string())?
                .current_state
                == ServiceState::Stopped
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    service.delete().map_err(|e| format!("delete: {e}"))?;
    println!("uninstalled '{SERVICE_NAME}'");

    let data_dir = config::default_data_dir();
    if purge_data {
        std::fs::remove_dir_all(&data_dir).map_err(|e| format!("purge: {e}"))?;
        println!("purged {}", data_dir.display());
    } else {
        println!(
            "kept {} — index snapshots (every indexed file name), logs and \
             service.json remain; rerun with --purge-data to remove them",
            data_dir.display()
        );
    }
    Ok(())
}

fn start_service() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("SCM open: {}", error_chain(&e)))?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::START)
        .map_err(|e| format!("open service (installed?): {}", error_chain(&e)))?;
    match service.start(&[] as &[&str]) {
        Ok(()) => {}
        // ERROR_SERVICE_ALREADY_RUNNING(1056): starting is idempotent too.
        Err(e) if raw_os_error(&e) == Some(1056) => {
            println!("'{SERVICE_NAME}' is already running");
            return Ok(());
        }
        Err(e) => return Err(format!("start: {}", error_chain(&e))),
    }
    println!("started '{SERVICE_NAME}'");
    Ok(())
}

/// The OS error behind a windows-service wrapper error, when there is one
/// (the crate's Display hides it — "IO error in winapi call").
fn raw_os_error(e: &windows_service::Error) -> Option<i32> {
    use std::error::Error as _;
    e.source()?.downcast_ref::<std::io::Error>()?.raw_os_error()
}

fn stop_service() -> Result<(), String> {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| e.to_string())?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
        )
        .map_err(|e| e.to_string())?;
    let _ = service.stop().map_err(|e| e.to_string())?;
    for _ in 0..100 {
        if service
            .query_status()
            .map_err(|e| e.to_string())?
            .current_state
            == ServiceState::Stopped
        {
            println!("stopped '{SERVICE_NAME}' (snapshots flushed)");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("service did not stop within 20s".into())
}

fn status() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|m| m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS))
        .and_then(|s| s.query_status())
    {
        Ok(st) => println!("SCM: {:?}", st.current_state),
        Err(e) => println!("SCM: not installed ({e})"),
    }

    // Live handshake — the first diagnostic for "is anything answering?".
    match ping(fmf_proto::PIPE_NAME) {
        Ok((pid, abi)) => println!("pipe: serving (pid {pid}, abi {abi})"),
        Err(e) => println!("pipe: no answer ({e})"),
    }
    Ok(())
}

fn ping(pipe_name: &str) -> std::io::Result<(u32, u32)> {
    use fmf_proto::frame::{FrameHeader, read_frame, write_frame};
    use fmf_proto::messages::{HelloReq, HelloResp, opcode};

    let mut s = PipeStream::connect(pipe_name)?;
    write_frame(
        &mut s,
        FrameHeader {
            len: 0,
            opcode: opcode::HELLO,
            flags: 0,
            request_id: 1,
            status: 0,
        },
        &HelloReq {
            protocol_version: fmf_proto::PROTOCOL_VERSION,
        }
        .encode(),
    )
    .map_err(std::io::Error::other)?;
    let (_, payload) = read_frame(&mut s).map_err(std::io::Error::other)?;
    let resp = HelloResp::decode(&payload).map_err(std::io::Error::other)?;
    Ok((resp.server_pid, resp.abi_version))
}

// ── Raw SERVICE_CONFIG_* the wrapper crate does not cover ───────────────

fn set_required_privileges(privs: &[&str]) -> Result<(), String> {
    use windows_sys::Win32::System::Services::{
        SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
        SERVICE_REQUIRED_PRIVILEGES_INFOW,
    };
    let mut multi: Vec<u16> = Vec::new();
    for p in privs {
        multi.extend(p.encode_utf16());
        multi.push(0);
    }
    multi.push(0);
    let info = SERVICE_REQUIRED_PRIVILEGES_INFOW {
        pmszRequiredPrivileges: multi.as_ptr() as *mut u16,
    };
    change_config2(
        SERVICE_CHANGE_CONFIG,
        SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
        (&info as *const SERVICE_REQUIRED_PRIVILEGES_INFOW).cast(),
    )
    .map_err(|e| format!("required privileges: {e}"))
}

fn set_preshutdown_timeout(timeout: Duration) -> Result<(), String> {
    use windows_sys::Win32::System::Services::{
        SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_PRESHUTDOWN_INFO, SERVICE_PRESHUTDOWN_INFO,
    };
    let info = SERVICE_PRESHUTDOWN_INFO {
        dwPreshutdownTimeout: timeout.as_millis() as u32,
    };
    change_config2(
        SERVICE_CHANGE_CONFIG,
        SERVICE_CONFIG_PRESHUTDOWN_INFO,
        (&info as *const SERVICE_PRESHUTDOWN_INFO).cast(),
    )
    .map_err(|e| format!("preshutdown timeout: {e}"))
}

fn change_config2(access: u32, level: u32, info: *const core::ffi::c_void) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Services::{
        ChangeServiceConfig2W, CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT,
    };
    let name: Vec<u16> = SERVICE_NAME.encode_utf16().chain([0]).collect();
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err(std::io::Error::from_raw_os_error(GetLastError() as i32));
        }
        let svc = OpenServiceW(scm, name.as_ptr(), access);
        if svc.is_null() {
            let e = std::io::Error::from_raw_os_error(GetLastError() as i32);
            CloseServiceHandle(scm);
            return Err(e);
        }
        let ok = ChangeServiceConfig2W(svc, level, info as *mut core::ffi::c_void);
        let err = GetLastError();
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        if ok == 0 {
            return Err(std::io::Error::from_raw_os_error(err as i32));
        }
    }
    Ok(())
}
