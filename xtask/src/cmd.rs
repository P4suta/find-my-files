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

/// Like [`run`], but with extra environment variables set on the child. The
/// shell-agnostic way to pass a value to a subprocess: `Command::env` sets it
/// directly, so it never passes through a shell that could mangle it —
/// powershell.exe strips the nested quotes from `cargo --config 'env.X="1"'`,
/// leaving a bare `1` that cargo rejects, which is exactly why the elevated
/// test recipe used to hard-code a PowerShell `$env:` assignment.
pub fn run_env(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .envs(envs.iter().copied())
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

/// Run `program args…` in `dir`, capturing its standard output. Returns the
/// trimmed output on success, or `None` when the program is missing or exits
/// non-zero — for version probes (`rustc --version`) where absence is a normal
/// answer rather than an error to bail on.
#[must_use]
pub fn capture(dir: &Path, program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
