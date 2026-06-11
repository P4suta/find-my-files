//! `diag` — versions, log locations and the in-process diagnostics ring.

pub fn diag() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "fmf {} ({})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::ARCH
    );
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    println!(r"engine log : {program_data}\find-my-files\logs\engine.log");
    println!(r"app log    : %APPDATA%\find-my-files\logs\app.log");
    println!("log filter : FMF_LOG (env var, e.g. FMF_LOG=debug)");
    let errors = fmf_core::diag::recent_errors();
    println!("recent in-process diagnostics ({}):", errors.len());
    println!("{}", serde_json::to_string_pretty(&errors)?);
    Ok(())
}
