//! Thin wrappers around external tools (git, dotnet). All run with an explicit
//! working directory — the commands must behave the same no matter where the
//! caller invoked xtask from.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Run `program args…` in `dir`, inheriting stdio; bail on a non-zero exit.
pub fn run(dir: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("failed to spawn `{program}` (is it on PATH?)"))?;
    if !status.success() {
        bail!("`{program} {}` exited with {status}", args.join(" "));
    }
    Ok(())
}

/// Run silently and report whether it succeeded — for probes like
/// `git rev-parse --verify <tag>` where a non-zero exit is a normal answer.
pub fn succeeds(dir: &Path, program: &str, args: &[&str]) -> Result<bool> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to spawn `{program}` (is it on PATH?)"))?;
    Ok(status.success())
}
