//! `diag` — versions, log locations and the in-process diagnostics ring.

use super::ctx::Ctx;
use super::json;

/// The machine-readable shape of `diag --format json`.
#[derive(serde::Serialize)]
struct DiagReport {
    version: &'static str,
    arch: &'static str,
    engine_log: String,
    app_log: String,
    log_filter: &'static str,
    recent_errors: serde_json::Value,
}

pub fn diag(ctx: Ctx) -> Result<(), Box<dyn std::error::Error>> {
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let engine_log = format!(r"{program_data}\find-my-files\logs\engine.log");
    let app_log = r"%APPDATA%\find-my-files\logs\app.log".to_owned();
    let errors = fmf_core::diag::recent_errors();

    if ctx.is_json() {
        return json::emit(&DiagReport {
            // Channel-aware build identity, identical to `fmf --version` (clap reads
            // the same const) — not the bare CARGO_PKG_VERSION, which dropped the
            // channel/sha and disagreed with --version.
            version: fmf_buildstamp::VERSION,
            arch: std::env::consts::ARCH,
            engine_log,
            app_log,
            log_filter: "FMF_LOG",
            recent_errors: serde_json::to_value(&errors)?,
        });
    }

    println!(
        "fmf {} ({})",
        fmf_buildstamp::VERSION,
        std::env::consts::ARCH
    );
    println!("engine log : {engine_log}");
    println!("app log    : {app_log}");
    println!("log filter : FMF_LOG (env var, e.g. FMF_LOG=debug)");
    println!("recent in-process diagnostics ({}):", errors.len());
    println!("{}", serde_json::to_string_pretty(&errors)?);
    Ok(())
}
