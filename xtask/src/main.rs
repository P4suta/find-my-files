//! find-my-files dev-task runner (the cargo-xtask pattern).
//!
//! Holds the imperative build/release plumbing that used to be inline
//! PowerShell in justfile and the GitHub workflows. `just` calls into here via
//! `cargo run --manifest-path xtask/Cargo.toml -- <cmd>`; the logic is plain
//! testable Rust instead of shell.

mod cmd;
mod fsx;
mod paths;

mod checksum;
mod locale;
mod semver;
mod version;

mod clean;
mod csharp_docs;
mod docs;
mod doctor;
mod package;
mod publish;
mod release;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "find-my-files build/release plumbing")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Bump the version (Rust workspace + C# app in lockstep), commit, and
    /// create a signed vX.Y.Z tag. Pushing the tag fires release.yml.
    Release {
        /// New semver, e.g. 0.2.0
        version: String,
        /// Rewrite the version files and show the diff without committing/tagging.
        #[arg(long)]
        dry_run: bool,
    },
    /// Assemble the distributable bundle in dist/FindMyFiles (publish the app,
    /// prune locales, copy the engine binaries, then self-verify the bundle).
    Publish {
        /// Skip the in-build cargo step (CI: engine binaries are prebuilt).
        #[arg(long, action = clap::ArgAction::Set, default_value_t = false)]
        skip_rust: bool,
    },
    /// Zip + checksum the assembled bundle for a release tag (vX.Y.Z).
    Package {
        /// The release tag, e.g. v0.2.0 (a leading 'v' is optional).
        tag: String,
    },
    /// Sweep leftover test fixtures (engine/target/test-tmp).
    CleanTemp,
    /// Stage generated docs (mdBook + rustdoc) into site/ for GitHub Pages.
    DocsAssemble,
    /// Generate the C# API reference (`DefaultDocumentation` -> `mdBook`) into
    /// build/docs-csharp/_site. The caller builds the app + restores tools first.
    DocCsharp,
    /// Check that the dev environment matches the `mise.toml` pins and the gate
    /// prerequisites (tool versions, lefthook, elevation, the build/ layout).
    Doctor,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Release { version, dry_run } => release::run(&version, dry_run),
        Commands::Publish { skip_rust } => publish::run(skip_rust),
        Commands::Package { tag } => package::run(&tag),
        Commands::CleanTemp => {
            clean::run();
            Ok(())
        }
        Commands::DocsAssemble => docs::run(),
        Commands::DocCsharp => csharp_docs::run(),
        Commands::Doctor => doctor::run(),
    }
}
