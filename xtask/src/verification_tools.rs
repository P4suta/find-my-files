use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{ensure, Context, Result};
use clap::ValueEnum;

use crate::{checksum::sha256_hex, cmd, paths};

const OCOMMENT_URL: &str = "https://github.com/P4suta/OComment";
const KANI_RELEASES_URL: &str = "https://github.com/model-checking/kani/releases/download";
const KANI_VERSION: &str = "0.68.0";
const KANI_SHA256: &str = "32e2b484d73ede0bbf64a2cf0879c4259422497d8aec4ee67d448de9ae7843d3";
const KANI_BUNDLE_BYTES: u64 = 140_315_343;
const COMMIT_HEX_BYTES: usize = 40;

#[derive(Clone, Copy, ValueEnum)]
pub enum Tool {
    Comments,
    Kani,
}

fn expose(directory: &Path) -> Result<()> {
    if let Some(path) = std::env::var_os("GITHUB_PATH") {
        writeln!(
            OpenOptions::new().append(true).open(path)?,
            "{}",
            directory.display()
        )?;
    }
    println!("Tool directory: {}", directory.display());
    Ok(())
}

fn install_comments(destination: &Path) -> Result<()> {
    let root = paths::repo_root();
    let pins = fs::read_to_string(root.join("mise.toml"))?.parse::<toml_edit::DocumentMut>()?;
    let tool_key = format!("cargo:{OCOMMENT_URL}");
    let version = pins["tools"][&tool_key]["version"]
        .as_str()
        .context("missing OComment source pin")?;
    let commit = version
        .strip_prefix("rev:")
        .context("OComment requires an immutable source revision")?;
    ensure!(
        commit.len() == COMMIT_HEX_BYTES
            && commit
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "invalid OComment commit"
    );
    let status = Command::new("cargo")
        .current_dir(&root)
        .args([
            "install",
            "--locked",
            "--profile",
            "dev",
            "--git",
            OCOMMENT_URL,
            "--rev",
            commit,
            "--root",
        ])
        .arg(destination)
        .arg("ocomment")
        .status()?;
    ensure!(status.success(), "OComment installation failed: {status}");
    Ok(())
}

fn install_kani(destination: &Path) -> Result<()> {
    let root = paths::repo_root();
    let pins = fs::read_to_string(root.join("mise.toml"))?.parse::<toml_edit::DocumentMut>()?;
    ensure!(
        pins["tools"]["cargo:kani-verifier"]["version"].as_str() == Some(KANI_VERSION),
        "Kani bundle metadata needs updating for the tool pin"
    );
    let install = Command::new("cargo")
        .current_dir(&root)
        .args([
            "install",
            "--locked",
            "--version",
            KANI_VERSION,
            "kani-verifier",
            "--root",
        ])
        .arg(destination)
        .status()?;
    ensure!(
        install.success(),
        "Kani bootstrap installation failed: {install}"
    );
    let archive = paths::verification_dir().join(format!(
        "kani-{KANI_VERSION}-x86_64-unknown-linux-gnu.tar.gz"
    ));
    let url = format!("{KANI_RELEASES_URL}/kani-{KANI_VERSION}/kani-{KANI_VERSION}-x86_64-unknown-linux-gnu.tar.gz");
    if !archive.exists() {
        let status = Command::new("curl")
            .current_dir(&root)
            .args([
                "--fail",
                "--location",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--retry",
                "2",
                "--max-time",
                "120",
                "--output",
            ])
            .arg(&archive)
            .arg(&url)
            .status()?;
        ensure!(status.success(), "Kani bundle download failed: {status}");
    }
    ensure!(
        fs::metadata(&archive)?.len() == KANI_BUNDLE_BYTES,
        "unexpected Kani bundle size"
    );
    ensure!(
        sha256_hex(&fs::read(&archive)?) == KANI_SHA256,
        "Kani bundle checksum mismatch"
    );
    let bootstrap = destination.join("bin/cargo-kani");
    let status = Command::new(&bootstrap)
        .current_dir(&root)
        .args(["setup", "--use-local-bundle"])
        .arg(&archive)
        .status()?;
    ensure!(status.success(), "Kani setup failed: {status}");
    cmd::run(
        &root,
        bootstrap.to_str().context("invalid Kani path")?,
        &["--version"],
    )
}

pub fn install(tool: Tool) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "CI tool provisioning requires Linux x64; local machines use mise install"
    );
    let destination: PathBuf = paths::verification_dir().join("tools");
    fs::create_dir_all(&destination)?;
    match tool {
        Tool::Comments => install_comments(&destination)?,
        Tool::Kani => install_kani(&destination)?,
    }
    expose(&destination.join("bin"))
}
