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
mod msix;
mod package;
mod publish;
mod signing;
mod test_admin;

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
    /// Print the canonical channel-aware version string — the source of the
    /// `FMF_BUILD_VERSION` format that the build stamp and nightly packaging use.
    /// Release *bumping* is release-please's job, not this command's.
    Version {
        /// Build channel: dev | nightly | stable.
        #[arg(long, default_value = "dev")]
        channel: String,
        /// YYYYMMDD date stamp (required for the nightly channel).
        #[arg(long)]
        date: Option<String>,
    },
    /// Assemble the distributable bundle in dist/FindMyFiles (publish the app
    /// into app/, prune locales, copy the engine binaries, add the root launcher
    /// + README, then self-verify the bundle).
    Publish {
        /// Skip the in-build cargo step (CI: engine binaries are prebuilt).
        #[arg(long, action = clap::ArgAction::Set, default_value_t = false)]
        skip_rust: bool,
    },
    /// Zip + checksum the assembled bundle. With a vX.Y.Z tag → stable zip; omit
    /// the tag for a nightly, whose name comes from `FMF_BUILD_VERSION`.
    Package {
        /// The release tag, e.g. v0.2.0 (a leading 'v' is optional). Omit for nightly.
        tag: Option<String>,
    },
    /// Build the installed-experience `.msix` from the assembled bundle (ADR-0028:
    /// packaged UI + unpackaged service). Run after `just publish`; MSIX ships the
    /// stable channel only, so a vX.Y.Z tag is required.
    PackageMsix {
        /// The release tag, e.g. v0.2.0 (a leading 'v' is optional).
        tag: Option<String>,
    },
    /// Sweep leftover test fixtures (engine/target/test-tmp).
    CleanTemp,
    /// Stage the bundle's first-party PEs into a flat dir for the release
    /// signing step (unique names; the eSigner Action signs them in place).
    SignStage,
    /// Copy the signed PEs back over the bundle after the signing step.
    SignCollect,
    /// Stage the packed `.msix` for the release signing step (a second eSigner
    /// pass — the wrapper is packed after its payload PEs are signed).
    SignStageMsix,
    /// Copy the signed `.msix` back into build/package after the signing step.
    SignCollectMsix,
    /// Run the elevated, `#[ignore]`-gated engine tests with `FMF_ADMIN_TESTS=1`.
    TestAdmin,
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
        Commands::Version { channel, date } => version::run(&channel, date.as_deref()),
        Commands::Publish { skip_rust } => publish::run(skip_rust),
        Commands::Package { tag } => package::run(tag.as_deref()),
        Commands::PackageMsix { tag } => msix::run(tag.as_deref()),
        Commands::CleanTemp => {
            clean::run();
            Ok(())
        }
        Commands::SignStage => signing::run_stage(),
        Commands::SignCollect => signing::run_collect(),
        Commands::SignStageMsix => signing::run_stage_msix(),
        Commands::SignCollectMsix => signing::run_collect_msix(),
        Commands::TestAdmin => test_admin::run(),
        Commands::DocsAssemble => docs::run(),
        Commands::DocCsharp => csharp_docs::run(),
        Commands::Doctor => doctor::run(),
    }
}
