//! fmf-service entry: console `run` (dev loop / test harness), the hidden
//! SCM entry, and the lifecycle subcommands. `install` is a subcommand and
//! not an sc.exe one-liner because it must coordinate four things:
//! capture the installing user's SID into service.json, harden the data-dir
//! DACLs, register with on-demand (demand) start + crash recovery, and set the
//! preshutdown/privilege configs (ADR-0017) plus the service-object DACL and GC
//! task (ADR-0027).

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use fmf_core::diag::error_chain;
use fmf_service::pipe::PipeStream;
use fmf_service::svc::{EXIT_LOCKED, SERVICE_NAME, ServeOptions};
use fmf_service::{config, lifecycle, security, svc};

#[derive(Parser)]
#[command(
    name = "fmf-service",
    version = fmf_buildstamp::VERSION,
    about = "find-my-files engine service"
)]
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
        /// Skip the startup `index_start` (unelevated pipe-surface debugging).
        #[arg(long)]
        no_index: bool,
    },
    /// Register the Windows service (elevated; captures your SID, hardens
    /// the data dir, sets on-demand start + crash recovery + preshutdown, and
    /// grants the user unelevated start/stop + the daily GC task, ADR-0027).
    Install {
        /// Also authorize this user SID on the pipe. The unelevated app
        /// forwards the daily user's SID here so OTS elevation (install runs
        /// as a *different* admin account) does not lock that user out of its
        /// own service (docs/SECURITY.md threat 1). Validated before trusting.
        #[arg(long)]
        owner_sid: Option<String>,
    },
    /// Install (idempotent) then restart, in one elevated step — the
    /// unelevated app's per-action-UAC "register / re-register" button. Equivalent
    /// to `install [--owner-sid] && restart`, so the freshly written
    /// authorized-SID list takes effect without a second UAC prompt.
    Setup {
        #[arg(long)]
        owner_sid: Option<String>,
    },
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
    /// Stop (if running) then start, so a rewritten service.json — e.g. a
    /// freshly added authorized SID — takes effect. The service reads its
    /// config only at startup, so an in-place `install` alone does not apply.
    Restart,
    /// SCM state + a live pipe handshake ping.
    Status,
    /// (internal) Daily GC: uninstall the on-demand service when it has gone
    /// unused past the idle threshold (ADR-0027). Run by the SYSTEM Scheduled
    /// Task that `install` registers; a no-op while the install is in use.
    Gc {
        /// Override the `service.json` `gc_max_idle_days` threshold (0 = never).
        #[arg(long)]
        max_idle_days: Option<u64>,
    },
    /// (internal) SCM entry point — launched by the service controller.
    #[command(hide = true)]
    ServiceEntry,
}

static CONSOLE_STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

fn main() -> std::process::ExitCode {
    let ok = match Cli::parse() {
        Cli::Run {
            pipe_name,
            data_dir,
            debug_faults,
            no_index,
        } => return run_console(pipe_name, data_dir, debug_faults, no_index),
        Cli::ServiceEntry => svc::dispatch().is_ok(),
        Cli::Install { owner_sid } => report(install(owner_sid)),
        Cli::Setup { owner_sid } => report(setup(owner_sid)),
        Cli::Uninstall { purge_data } => report(uninstall(purge_data)),
        Cli::Start => report(start_service()),
        Cli::Stop => report(stop_service()),
        Cli::Restart => report(restart_service()),
        Cli::Status => report(status()),
        Cli::Gc { max_idle_days } => report(gc(max_idle_days)),
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
    let data_dir = match data_dir {
        Some(path) => path,
        None => match config::default_data_dir() {
            Ok(path) => path,
            Err(e) => {
                eprintln!("fmf-service: ProgramData resolution failed: {e}");
                return std::process::ExitCode::FAILURE;
            }
        },
    };
    let config_path = data_dir.join("service.json");
    let (cfg, config_warning) = config::ServiceConfig::load_or_default_with_error(&config_path);
    fmf_core::diag::init_diag(
        Some(&data_dir.join("logs")),
        &cfg.log_level,
        fmf_core::diag::SERVICE_MAX_LOG_FILES,
    );
    if let Some(error) = config_warning {
        tracing::warn!(
            path = %config_path.display(),
            %error,
            "service.json unreadable — console defaults"
        );
    }
    let stop = Arc::new(AtomicBool::new(false));
    if CONSOLE_STOP.set(stop.clone()).is_err() {
        tracing::error!("console stop signal was already initialized");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(e) = install_ctrl_c() {
        tracing::error!(error = %e, "Ctrl+C handler registration failed");
        return std::process::ExitCode::FAILURE;
    }

    let ready_pipe_name = pipe_name.clone();
    match svc::serve(
        &ServeOptions {
            data_dir,
            data_root: None,
            pipe_name,
            debug_faults,
            no_index,
            require_authorization: false,
            client_verifier: security::verify_client,
        },
        &stop,
        || {
            println!("fmf-service: serving on {ready_pipe_name} (Ctrl+C to stop)");
            Ok(())
        },
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

fn install_ctrl_c() -> std::io::Result<()> {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(_ctrl_type: u32) -> i32 {
        if let Some(stop) = CONSOLE_STOP.get() {
            stop.store(true, Ordering::Relaxed);
        }
        1 // handled — give main time to flush
    }
    if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ── Lifecycle subcommands ───────────────────────────────────────────────

fn install(owner_sid: Option<String>) -> Result<(), String> {
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceInfo, ServiceStartType, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    // Pin the running image before any blocking account/service/filesystem
    // work. The unelevated UI additionally holds its verified image and parent
    // directory leases until this child exits (SECURITY.md threat 10); this
    // service-side guard ensures all later identity checks and copying use the
    // same file object rather than resolving this path again.
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let current_image = security::TrustedSourceFile::open(&current_exe).map_err(|e| {
        format!(
            "lock current service image ({}): {e}",
            current_exe.display()
        )
    })?;

    // 1. Capture the installing user — the one identity allowed on the pipe.
    let sid = security::current_user_sid().map_err(|e| format!("SID capture: {e}"))?;
    // A forwarded owner SID (OTS elevation runs install as a *different* admin
    // than the daily user) must be validated before touching the machine-wide
    // tree. Account lookup may block; do not leave a newly created,
    // ProgramData-inherited directory writable during that work.
    let owner_sid = match owner_sid.filter(|owner| owner != &sid) {
        Some(owner)
            if security::validate_user_sid(&owner)
                .map_err(|e| format!("--owner-sid validation failed: {}", error_chain(&e)))? =>
        {
            Some(owner)
        }
        Some(_) => return Err("--owner-sid is not a real user account".into()),
        None => None,
    };
    let mut authorized_sids = vec![sid.clone()];
    if let Some(owner) = &owner_sid
        && owner != &sid
    {
        authorized_sids.push(owner.clone());
    }

    // 2. Disable first, then remove user start rights, then stop. ACL changes do
    //    not revoke an already-open SERVICE_START handle, but SERVICE_DISABLED
    //    is enforced when that handle calls StartService. Any start that won the
    //    race just before disable is covered by the subsequent stop/wait.
    //    Failure leaves the service disabled/restricted rather than restoring a
    //    partially updated runnable LocalSystem registration.
    quarantine_service_for_maintenance()?;
    // Retire the old task now. If deletion fails, continue only through root
    // validation/rotation so its fixed action can no longer resolve an
    // attacker-owned image, then abort: an existing task object is never
    // "repaired" in place.
    let old_task_delete_error = delete_gc_task().err();

    // 3. Create and pin the data tree before the SCM knows about a new image.
    //    TrustedDataRoot keeps both ProgramData and the fixed child open for the
    //    complete ritual, rejects pre-open mutation handles/reparse points/hard
    //    links, and applies security to the same handles it verified.
    let data_dir =
        config::default_data_dir().map_err(|e| format!("ProgramData resolution: {e}"))?;
    let protected_sddl = security::data_dir_sddl();
    let data_root = match security::TrustedDataRoot::create_or_harden_machine(&protected_sddl) {
        Ok(root) => root,
        Err(error) => {
            let task = match old_task_delete_error.as_ref() {
                Some(task) => format!("; old GC task deletion also failed: {task}"),
                None => String::new(),
            };
            return Err(format!("trusted data root: {error}{task}"));
        }
    };
    if let Some(quarantined) = data_root.quarantined_root() {
        eprintln!(
            "fmf-service: legacy/provenance-less data was not trusted and was quarantined at {}; the index will be rebuilt",
            quarantined.display()
        );
    }
    if let Some(task) = old_task_delete_error {
        return Err(format!(
            "old GC task deletion failed: {task}; the service remains retired and the fixed data root is now trusted, but setup will not repair an existing task object in place"
        ));
    }
    data_root
        .ensure_directory("index", &protected_sddl)
        .map_err(|e| format!("index directory: {e}"))?;
    // Create logs protected first; its narrowly-scoped read ACE is applied
    // only after the forwarded/installer SID set is fully validated.
    data_root
        .ensure_directory("logs", &protected_sddl)
        .map_err(|e| format!("logs directory: {e}"))?;

    // 4. Harden the tree (SECURITY.md threat 7): the data root AND index/ —
    //    machine-wide file-name snapshots — are SYSTEM+Administrators only; logs/
    //    additionally grants the installing admin + forwarded owner read for the
    //    F12 copy path. Every mutation is attached to the verified handle, not a
    //    check-then-reopened path. The (subdir, SDDL) policy lives in the
    //    unit-pinned security::data_tree_security_descriptors builder.
    let log_readers: Vec<_> = authorized_sids.iter().map(String::as_str).collect();
    for (sub, sddl) in security::data_tree_security_descriptors(&log_readers) {
        let result = if sub.is_empty() {
            data_root.set_root_security(&sddl)
        } else {
            data_root.harden_tree(sub, &sddl)
        };
        let what = if sub.is_empty() { "data dir" } else { sub };
        result.map_err(|e| format!("{what} security: {e}"))?;
    }
    let mut cfg = match data_root
        .open_file_read("service.json")
        .and_then(config::ServiceConfig::try_load_file)
    {
        Ok(config) => config,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => config::ServiceConfig::default(),
        Err(e) => return Err(format!("service.json is invalid; refusing overwrite: {e}")),
    };
    // Never merge an existing allowlist. A valid JSON file may predate owner
    // hardening; only identities validated in this install ritual are trusted.
    cfg.authorized_sids.clone_from(&authorized_sids);
    let config_bytes = cfg
        .to_json_bytes()
        .map_err(|e| format!("service.json serialization: {e}"))?;
    data_root
        .atomic_write("service.json", &config_bytes, &protected_sddl)
        .map_err(|e| format!("service.json publication: {e}"))?;
    data_root
        .harden_file_if_exists("last_use", &protected_sddl)
        .map_err(|e| format!("last_use security: {e}"))?;

    // 4b. Copy fmf-service.exe out of the (portable) app bundle into the
    //     hardened data root, and point the registration + GC task at this
    //     stable copy (ADR-0027): both then survive the app folder being
    //     deleted, and a standard user — who cannot write the SYSTEM+Admins
    //     data root — cannot replace the SYSTEM binary (docs/SECURITY.md). The
    //     publication uses a protected staging handle and FILE_RENAME_INFO
    //     relative to the pinned root handle, so neither source completion nor
    //     destination replacement depends on a re-opened path.
    let stable_exe = lifecycle::stable_exe_path(&data_dir);
    if !data_root
        .child_is_same_file("fmf-service.exe", &current_image)
        .map_err(|e| format!("stable exe identity: {e}"))?
    {
        data_root
            .atomic_copy("fmf-service.exe", &current_image, &protected_sddl)
            .map_err(|e| {
                format!(
                    "stable exe copy ({} → {}): {e}",
                    current_exe.display(),
                    stable_exe.display()
                )
            })?;
    }
    data_root
        .harden_file_if_exists("fmf-service.exe", &protected_sddl)
        .map_err(|e| format!("stable exe security: {e}"))?;
    // The user SIDs allowed to start/stop the service unelevated (ADR-0027) —
    // the same identities authorized on the pipe.
    // 4. Register: LocalSystem, on-demand (manual) start, restart-on-crash.
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .map_err(|e| format!("SCM open (elevated?): {}", error_chain(&e)))?;
    let service_info = ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: "find-my-files engine".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::Disabled,
        error_control: ServiceErrorControl::Normal,
        executable_path: stable_exe.clone(),
        launch_arguments: vec!["service-entry".into()],
        dependencies: vec![],
        // Keep the recreated object unambiguously bound to LocalSystem rather
        // than relying on wrapper/default-account behavior.
        account_name: Some("LocalSystem".into()),
        account_password: Some("".into()),
    };
    let service = match manager.create_service(
        &service_info,
        ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::DELETE
            | ServiceAccess::WRITE_DAC
            | ServiceAccess::WRITE_OWNER
            | ServiceAccess::READ_CONTROL,
    ) {
        Ok(service) => service,
        Err(error) if matches!(raw_os_error(&error), Some(1072 | 1073)) => {
            return Err(format!(
                "the retired service object still has an open handle; close find-my-files processes and rerun setup (SCM: {})",
                error_chain(&error)
            ));
        }
        Err(error) => return Err(format!("create_service: {}", error_chain(&error))),
    };
    let configure = (|| -> Result<(), String> {
        // `CreateService` does not accept an explicit security descriptor. Close
        // its unavoidable default-descriptor interval immediately, before any
        // other fallible work: keep the disabled object writable only by
        // SYSTEM/Administrators, then add the validated start/stop principals at
        // the final commit boundary below.
        security::set_service_handle_security(&service, &security::service_sddl(&[]))
            .map_err(|e| format!("initial service owner/group/DACL: {e}"))?;
        service
            .set_description(fmf_proto::SERVICE_PROTOCOL_MARKER)
            .map_err(|e| format!("service protocol marker: {}", error_chain(&e)))?;
        service
            .update_failure_actions(ServiceFailureActions {
                reset_period: windows_service::service::ServiceFailureResetPeriod::After(
                    Duration::from_hours(24),
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
        set_required_privileges(&service, &["SeChangeNotifyPrivilege"])?;
        set_preshutdown_timeout(&service, Duration::from_mins(3))?;

        // 6. Finish every fallible machine mutation while the service remains
        //    disabled and restricted to SYSTEM/Administrators.
        if cfg.gc_max_idle_days > 0 {
            register_gc_task(&data_root, &stable_exe, &protected_sddl)?;
        } else {
            delete_gc_task()?;
        }
        security::set_service_handle_security(&service, &security::service_sddl(&authorized_sids))
            .map_err(|e| format!("service DACL: {e}"))?;
        // Absolute commit point: only after config/image/task/DACL are complete
        // does the LocalSystem service become startable again.
        set_start_type_demand(&service)?;
        Ok(())
    })();
    if let Err(primary) = configure {
        // An install must not leave a startable, partially configured
        // LocalSystem service. The create handle retained DELETE before the
        // service DACL was narrowed, so rollback remains possible at every
        // later step. Secure data files are deliberately retained: a retry is
        // idempotent, and recursively deleting a machine data root here would
        // risk unrelated pre-existing index data.
        let mut rollback_errors = Vec::new();
        if let Err(e) = delete_gc_task() {
            rollback_errors.push(format!("task rollback: {e}"));
        }
        if let Err(e) = service.delete() {
            rollback_errors.push(format!("service rollback: {}", error_chain(&e)));
        }
        if rollback_errors.is_empty() {
            return Err(format!(
                "{primary}; rolled back the newly created service/task (secure data retained for retry)"
            ));
        }
        return Err(format!(
            "{primary}; new-install rollback incomplete: {}",
            rollback_errors.join("; ")
        ));
    }

    println!("installed '{SERVICE_NAME}' (LocalSystem, on-demand start)");
    println!("authorized SID: {sid}");
    if let Some(owner) = &owner_sid {
        println!("authorized SID (forwarded owner): {owner}");
    }
    println!("start it with: fmf-service start  (or `just service-start`)");
    Ok(())
}

/// `install` (idempotent) then `restart` in one elevated process, so the
/// unelevated app's "register / re-register" button is a single UAC prompt. The
/// authorized-SID list is read only at service startup, so the restart is
/// what actually applies a freshly installed SID — the same install+restart
/// pairing the in-app `InstallAndRestart` does, moved server-side. Covers both
/// first install (service stopped → restart just starts it) and re-register
/// (service running → restart re-reads service.json).
fn setup(owner_sid: Option<String>) -> Result<(), String> {
    install(owner_sid)?;
    restart_service()
}

fn uninstall(purge_data: bool) -> Result<(), String> {
    let data_dir =
        config::default_data_dir().map_err(|e| format!("ProgramData resolution: {e}"))?;
    // Remove the startable SYSTEM service first. Afterwards no authorized
    // standard user can restart it while the filesystem ritual takes locks.
    deregister_service_and_task()?;
    let protected_sddl = security::data_dir_sddl();
    let data_root = match security::TrustedDataRoot::open_and_harden_machine(&protected_sddl) {
        Ok(root) => Some(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("trusted data root: {error}")),
    };

    if purge_data {
        if let Some(root) = data_root {
            root.purge().map_err(|e| format!("purge: {e}"))?;
        }
        println!("purged {}", data_dir.display());
    } else {
        // Remove the stable binary copy too — it is program clutter, not user
        // data (a re-install copies it fresh from the bundle). Keep only the
        // index/logs/service.json the user may want to reuse; --purge-data
        // removes those as well. uninstall runs from the bundle exe, so the
        // stable copy is not in use and deletes cleanly (no reboot needed).
        if let Some(root) = data_root {
            root.remove_file_if_exists("fmf-service.exe")
                .map_err(|e| format!("remove stable service binary: {e}"))?;
        }
        println!(
            "kept {} — index snapshots (every indexed file name), logs and \
             service.json remain; rerun with --purge-data to remove them",
            data_dir.display()
        );
    }
    Ok(())
}

/// Stops (if running) and deletes the SCM service, then removes the GC
/// Scheduled Task. Shared by `uninstall` and `gc`.
fn deregister_service_and_task() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("SCM open: {e}"))?;
    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::DELETE | ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
    ) {
        Ok(service) => Some(service),
        Err(e) if raw_os_error(&e) == Some(1060) => None,
        Err(e) => return Err(format!("open service for delete: {}", error_chain(&e))),
    };
    if let Some(service) = &service {
        // Mark for deletion before stop/wait. Once marked, even an authorized
        // user holding a stale SERVICE_START handle cannot create a fresh
        // startable registration under this name; this retained handle remains
        // valid for the stop/status sequence.
        service.delete().map_err(|e| format!("delete: {e}"))?;
        stop_service_handle(service)?;
    }
    delete_gc_task()?;
    if service.is_some() {
        println!("uninstalled '{SERVICE_NAME}'");
    } else {
        println!("'{SERVICE_NAME}' was already uninstalled");
    }
    Ok(())
}

/// Daily GC entry (ADR-0027): when the install has gone unused past the idle
/// threshold, tear it down completely — service, Scheduled Task, index/logs/
/// config. The running stable binary and its (then-empty) directory cannot be
/// deleted while this process holds them, so they are scheduled for deletion on
/// the next reboot. Otherwise a no-op. Deliberately does NOT init the file log
/// (that would lock the logs dir this may delete).
fn gc(max_idle_days: Option<u64>) -> Result<(), String> {
    let data_dir =
        config::default_data_dir().map_err(|e| format!("ProgramData resolution: {e}"))?;
    // A running/pending service intentionally holds a non-delete-shared root
    // guard. Check SCM before requesting DELETE on that directory so the daily
    // task is a clean no-op, not a deterministic sharing violation.
    if service_is_active()? {
        println!("gc: service is active — nothing to do");
        return Ok(());
    }
    let protected_sddl = security::data_dir_sddl();
    // The GC binary runs as SYSTEM from inside this tree. Pin and verify the
    // root before reading age/config inputs; an unexpected descriptor is never
    // repaired in place and no check-then-path-open window is allowed.
    let data_root = match security::TrustedDataRoot::open_and_harden_machine(&protected_sddl) {
        Ok(root) => root,
        Err(error) if error.raw_os_error() == Some(32) && service_is_active()? => {
            println!("gc: service became active — nothing to do");
            return Ok(());
        }
        Err(error) => return Err(format!("trusted data root: {error}")),
    };
    let cfg = data_root
        .open_file_read("service.json")
        .and_then(config::ServiceConfig::try_load_file)
        .map_err(|e| format!("gc requires a valid service.json: {e}"))?;
    let threshold = max_idle_days.unwrap_or(cfg.gc_max_idle_days);
    let last_use = match data_root.open_file_read("last_use") {
        Ok(file) => Some(
            lifecycle::read_last_use_file(file, &lifecycle::last_use_path(&data_dir))
                .map_err(|e| format!("gc read last_use: {e}"))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("gc read last_use: {error}")),
    };
    if !lifecycle::gc_should_remove(std::time::SystemTime::now(), last_use, threshold) {
        println!("gc: in use or disabled (threshold {threshold}d) — nothing to do");
        return Ok(());
    }
    // `last_use` is stamped on connection/stop boundaries, not continuously.
    // A tray-resident UI may therefore keep one healthy pipe session open past
    // the age threshold. SCM state is the authoritative live lease: never stop
    // or deregister a service that is running/pending merely because its stamp
    // is old.
    if service_is_active()? {
        println!("gc: service is active — nothing to do");
        return Ok(());
    }

    println!("gc: unused > {threshold}d — removing the on-demand install");
    deregister_service_and_task()?;
    // The service is stopped now, so these are free to delete.
    for sub in ["index", "logs"] {
        data_root
            .remove_tree_if_exists(sub)
            .map_err(|e| format!("gc remove {sub}: {e}"))?;
    }
    data_root
        .remove_file_if_exists("service.json")
        .map_err(|e| format!("gc remove service.json: {e}"))?;
    data_root
        .remove_file_if_exists("last_use")
        .map_err(|e| format!("gc remove last_use: {e}"))?;
    // The running stable image (and its now-empty dir) self-delete on reboot.
    schedule_delete_on_reboot(&lifecycle::stable_exe_path(&data_dir))?;
    schedule_delete_on_reboot(&data_dir)?;
    println!("gc: done (binary + data dir scheduled for deletion on next reboot)");
    Ok(())
}

fn service_is_active() -> Result<bool, String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("gc SCM open: {}", error_chain(&e)))?;
    let service = match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => service,
        // Service already absent: stale data/task cleanup may proceed.
        Err(error) if raw_os_error(&error) == Some(1060) => return Ok(false),
        Err(error) => {
            return Err(format!(
                "gc open service for status: {}",
                error_chain(&error)
            ));
        }
    };
    let state = service
        .query_status()
        .map_err(|e| format!("gc query service status: {}", error_chain(&e)))?
        .current_state;
    Ok(service_state_is_active(state))
}

fn service_state_is_active(state: windows_service::service::ServiceState) -> bool {
    state != windows_service::service::ServiceState::Stopped
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

/// Stop (if running) then start, so the service re-reads service.json — the
/// authorized-SID list is consulted only at startup, so a fresh `install`
/// that adds a SID does nothing for a running instance until this runs.
fn restart_service() -> Result<(), String> {
    stop_service()?;
    start_service()
}

/// The OS error behind a windows-service wrapper error, when there is one
/// (the crate's Display hides it — "IO error in winapi call").
fn raw_os_error(e: &windows_service::Error) -> Option<i32> {
    use std::error::Error as _;
    e.source()?.downcast_ref::<std::io::Error>()?.raw_os_error()
}

fn stop_service() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("SCM open: {}", error_chain(&e)))?;
    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::QUERY_STATUS,
    ) {
        Ok(service) => service,
        Err(e) if raw_os_error(&e) == Some(1060) => {
            println!("'{SERVICE_NAME}' is not installed");
            return Ok(());
        }
        Err(e) => {
            return Err(format!("open service for stop: {}", error_chain(&e)));
        }
    };
    stop_service_handle(&service)
}

fn stop_service_handle(service: &windows_service::service::Service) -> Result<(), String> {
    use windows_service::service::ServiceState;

    let state = service
        .query_status()
        .map_err(|e| format!("query before stop: {}", error_chain(&e)))?
        .current_state;
    if state == ServiceState::Stopped {
        println!("'{SERVICE_NAME}' is already stopped");
        return Ok(());
    }
    if state != ServiceState::StopPending {
        service
            .stop()
            .map_err(|e| format!("stop: {}", error_chain(&e)))?;
    }
    for _ in 0..100 {
        if service
            .query_status()
            .map_err(|e| format!("query while stopping: {}", error_chain(&e)))?
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

#[expect(
    clippy::unnecessary_wraps,
    reason = "uniform Result<(), String> shape so every subcommand flows through report()"
)]
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

// ── On-demand lifecycle helpers (ADR-0027) ─────────────────────────────

/// Registers the daily GC Scheduled Task as SYSTEM. The XML definition (and the
/// UTF-16 encoding `schtasks` needs across locales) is built by
/// [`lifecycle::gc_task_xml`]; here we only drop it to a file and shell out.
fn register_gc_task(
    data_root: &security::TrustedDataRoot,
    stable_exe: &std::path::Path,
    protected_sddl: &str,
) -> Result<(), String> {
    let xml_path = data_root.path().join("gc-task.xml");
    data_root
        .atomic_write(
            "gc-task.xml",
            &lifecycle::gc_task_xml(stable_exe),
            protected_sddl,
        )
        .map_err(|e| format!("write task xml: {e}"))?;
    let schtasks = match trusted_schtasks_path() {
        Ok(path) => path,
        Err(primary) => {
            return match data_root.remove_file_if_exists("gc-task.xml") {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(format!(
                    "{primary}; task XML cleanup also failed: {cleanup}"
                )),
            };
        }
    };
    let status = std::process::Command::new(&schtasks)
        // The old object was synchronously removed and verified absent. Do not
        // pass /F: a racing replacement must fail instead of being updated in
        // place while a creator-owned task handle could retain control.
        .args(["/Create", "/TN", lifecycle::GC_TASK_NAME, "/XML"])
        .arg(&xml_path)
        .stdin(std::process::Stdio::null())
        .status();
    let cleanup = data_root.remove_file_if_exists("gc-task.xml");
    match status {
        Ok(s) if s.success() => {
            if let Err(security_error) = security::verify_gc_task_security() {
                let mut rollback_errors = Vec::new();
                if let Err(error) = cleanup {
                    rollback_errors.push(format!("task XML: {error}"));
                }
                if let Err(error) = delete_gc_task() {
                    rollback_errors.push(format!("registered task: {error}"));
                }
                if rollback_errors.is_empty() {
                    return Err(format!(
                        "registered GC task failed exact security validation and was removed: {security_error}"
                    ));
                }
                return Err(format!(
                    "registered GC task failed exact security validation: {security_error}; rollback incomplete: {}",
                    rollback_errors.join("; ")
                ));
            }
            cleanup.map_err(|e| format!("remove task XML: {e}"))?;
            println!("registered daily GC task '{}'", lifecycle::GC_TASK_NAME);
            Ok(())
        }
        Ok(s) => Err(match cleanup {
            Ok(()) => format!("schtasks /Create exited {}", s.code().unwrap_or(-1)),
            Err(e) => format!(
                "schtasks /Create exited {}; task XML cleanup also failed: {e}",
                s.code().unwrap_or(-1)
            ),
        }),
        Err(e) => Err(match cleanup {
            Ok(()) => format!("schtasks /Create: {e}"),
            Err(cleanup_error) => {
                format!("schtasks /Create: {e}; task XML cleanup also failed: {cleanup_error}")
            }
        }),
    }
}

/// Removes the GC Scheduled Task. `schtasks` uses exit 1 for both "not found"
/// and real failures, so the trusted Task Scheduler store is checked before
/// classifying it as an idempotent no-op.
fn delete_gc_task() -> Result<(), String> {
    let schtasks = trusted_schtasks_path()?;
    let status = std::process::Command::new(&schtasks)
        .args(["/Delete", "/F", "/TN", lifecycle::GC_TASK_NAME])
        .status()
        .map_err(|e| format!("schtasks /Delete: {e}"))?;
    let task_file = trusted_system_dir()?
        .join("Tasks")
        .join(lifecycle::GC_TASK_NAME);
    match std::fs::metadata(&task_file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if status.success() || status.code() == Some(1) {
                Ok(())
            } else {
                Err(format!(
                    "schtasks /Delete exited {} although {} is absent",
                    status.code().unwrap_or(-1),
                    task_file.display()
                ))
            }
        }
        Ok(_) => Err(format!(
            "schtasks /Delete exited {} while {} still exists",
            status.code().unwrap_or(-1),
            task_file.display()
        )),
        Err(e) => Err(format!(
            "schtasks /Delete exited {}; could not verify {} is absent: {e}",
            status.code().unwrap_or(-1),
            task_file.display()
        )),
    }
}

fn trusted_system_dir() -> Result<std::path::PathBuf, String> {
    config::system_dir().map_err(|e| format!("System32 resolution: {e}"))
}

fn trusted_schtasks_path() -> Result<std::path::PathBuf, String> {
    let path = trusted_system_dir()?.join("schtasks.exe");
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => Ok(path),
        Ok(_) => Err(format!(
            "trusted schtasks path is not a file: {}",
            path.display()
        )),
        Err(e) => Err(format!(
            "trusted schtasks unavailable ({}): {e}",
            path.display()
        )),
    }
}

fn quarantine_service_for_maintenance() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::System::Services::SERVICE_DISABLED;

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("maintenance SCM open: {}", error_chain(&e)))?;
    let service = match manager.open_service(
        SERVICE_NAME,
        ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::DELETE
            | ServiceAccess::STOP
            | ServiceAccess::QUERY_STATUS
            | ServiceAccess::WRITE_DAC
            | ServiceAccess::WRITE_OWNER
            | ServiceAccess::READ_CONTROL,
    ) {
        Ok(service) => service,
        Err(error) if raw_os_error(&error) == Some(1060) => return Ok(()),
        Err(error) => {
            return Err(format!(
                "open service for maintenance: {}",
                error_chain(&error)
            ));
        }
    };
    // Use this same retained object for the entire maintenance transaction.
    // Disabling blocks StartService even through a pre-open SERVICE_START handle.
    set_start_type_handle(service.raw_handle(), SERVICE_DISABLED)
        .map_err(|e| format!("disable old service for maintenance: {e}"))?;
    let security_error =
        security::set_service_handle_security(&service, &security::service_sddl(&[])).err();
    let delete_error = service.delete().err();
    let stop_error = stop_service_handle(&service).err();
    if security_error.is_none() && delete_error.is_none() && stop_error.is_none() {
        return Ok(());
    }
    Err(format!(
        "retire old service safely: {}",
        [
            security_error.map(|e| format!("restrict owner/group/DACL: {e}")),
            delete_error.map(|e| format!("mark delete: {}", error_chain(&e))),
            stop_error,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("; ")
    ))
}

/// Forces the final service start type to `DEMAND_START`. This is the install
/// transaction's last commit operation.
fn set_start_type_demand(service: &windows_service::service::Service) -> Result<(), String> {
    use windows_sys::Win32::System::Services::SERVICE_DEMAND_START;

    set_start_type_handle(service.raw_handle(), SERVICE_DEMAND_START)
}

fn set_start_type_handle(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
    start_type: u32,
) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Services::{ChangeServiceConfigW, SERVICE_NO_CHANGE};

    // SAFETY: callers retain a live service handle with CHANGE_CONFIG, all
    // optional string/buffer arguments are intentionally null, and no pointer
    // is retained by ChangeServiceConfigW after it returns.
    let ok = unsafe {
        ChangeServiceConfigW(
            service,
            SERVICE_NO_CHANGE,
            start_type,
            SERVICE_NO_CHANGE,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        return Err(
            std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).to_string(),
        );
    }
    Ok(())
}

/// Schedules `path` for deletion on the next reboot — the self-delete idiom for
/// the running GC binary and its directory (`MoveFileEx` + delay-until-reboot).
fn schedule_delete_on_reboot(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let ok = unsafe { MoveFileExW(wide.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
    if ok == 0 {
        let e = std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32);
        return Err(format!(
            "could not schedule {} for deletion: {e}",
            path.display()
        ));
    }
    Ok(())
}

// ── Raw SERVICE_CONFIG_* the wrapper crate does not cover ───────────────

fn set_required_privileges(
    service: &windows_service::service::Service,
    privs: &[&str],
) -> Result<(), String> {
    use windows_sys::Win32::System::Services::{
        SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO, SERVICE_REQUIRED_PRIVILEGES_INFOW,
    };
    let mut multi: Vec<u16> = Vec::new();
    for p in privs {
        multi.extend(p.encode_utf16());
        multi.push(0);
    }
    multi.push(0);
    let info = SERVICE_REQUIRED_PRIVILEGES_INFOW {
        pmszRequiredPrivileges: multi.as_ptr().cast_mut(),
    };
    change_config2_handle(
        service.raw_handle(),
        SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO,
        (&raw const info).cast(),
    )
    .map_err(|e| format!("required privileges: {e}"))
}

fn set_preshutdown_timeout(
    service: &windows_service::service::Service,
    timeout: Duration,
) -> Result<(), String> {
    use windows_sys::Win32::System::Services::{
        SERVICE_CONFIG_PRESHUTDOWN_INFO, SERVICE_PRESHUTDOWN_INFO,
    };
    let info = SERVICE_PRESHUTDOWN_INFO {
        dwPreshutdownTimeout: timeout.as_millis() as u32,
    };
    change_config2_handle(
        service.raw_handle(),
        SERVICE_CONFIG_PRESHUTDOWN_INFO,
        (&raw const info).cast(),
    )
    .map_err(|e| format!("preshutdown timeout: {e}"))
}

fn change_config2_handle(
    service: windows_sys::Win32::System::Services::SC_HANDLE,
    level: u32,
    info: *const core::ffi::c_void,
) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Services::ChangeServiceConfig2W;

    // SAFETY: `service` is the retained create/maintenance handle with
    // CHANGE_CONFIG; `info` points to the live level-specific struct and no
    // pointer is retained after this synchronous call.
    if unsafe { ChangeServiceConfig2W(service, level, info.cast_mut()) } == 0 {
        return Err(std::io::Error::from_raw_os_error(
            // SAFETY: queried immediately after the failed Win32 call.
            unsafe { GetLastError() } as i32,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_only_treats_a_fully_stopped_service_as_inactive() {
        use windows_service::service::ServiceState;

        assert!(!service_state_is_active(ServiceState::Stopped));
        for state in [
            ServiceState::StartPending,
            ServiceState::StopPending,
            ServiceState::Running,
            ServiceState::ContinuePending,
            ServiceState::PausePending,
            ServiceState::Paused,
        ] {
            assert!(service_state_is_active(state), "{state:?}");
        }
    }
}
