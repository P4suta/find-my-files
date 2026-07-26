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
mod notices;
mod pe_digest;
mod pe_load;
mod prune;
mod semver;
mod version;
mod win_version;

mod clean;
mod contract_bless;
mod docs;
mod doctor;
mod package;
mod perf;
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
    /// Assemble an isolated, non-shippable bundle with deterministic UI-test
    /// seams compiled in. Never writes to build/dist.
    PublishUiTest {
        /// Skip the in-build cargo step (CI: engine binaries are prebuilt).
        #[arg(long, action = clap::ArgAction::Set, default_value_t = false)]
        skip_rust: bool,
    },
    /// Zip + checksum the assembled bundle. With a vX.Y.Z tag → stable zip; omit
    /// the tag to name a dev/nightly zip from the bundled `BUILDINFO.txt`.
    Package {
        /// The release tag, e.g. v0.2.0 (a leading 'v' is optional). Omit for
        /// the bundle's stamped dev/nightly identity.
        tag: Option<String>,
    },
    /// Verify a release tag (vX.Y.Z) matches the committed workspace version —
    /// the manual-dispatch guard release.yml runs before signing/packaging so a
    /// drifted tag can't ship mislabeled artifacts.
    CheckVersion {
        /// The release tag, e.g. v0.2.0 (a leading 'v' is optional).
        tag: String,
    },
    /// Sweep leftover engine test fixtures under build/engine/test-tmp.
    CleanTemp,
    /// Stage the bundle's first-party PEs into a flat dir for the release
    /// signing step (unique names; the eSigner Action signs them in place).
    SignStage,
    /// Copy the signed PEs back over the bundle after the signing step.
    SignCollect,
    /// Run the elevated, `#[ignore]`-gated engine tests with `FMF_ADMIN_TESTS=1`.
    TestAdmin,
    /// Explicitly recapture the shared wire and JSON contract golden corpus.
    ContractBless,
    /// Stage the landing page and canonical mdBook into build/site.
    DocsAssemble,
    /// Refuse performance measurement unless Windows reports a cold, idle CPU.
    PerfPreflight,
    /// Compare the synthetic Criterion suite with the machine-local baseline
    /// inside a fresh, monitored run directory.
    PerfMicroCheck,
    /// Record and atomically promote a monitored Criterion baseline.
    PerfMicroBaseline,
    /// Run the monitored real-volume gate against the committed baseline.
    PerfRealCheck {
        /// NTFS volume to measure.
        #[arg(default_value = "C:")]
        drive: String,
    },
    /// Record and atomically promote the committed real-volume baseline.
    PerfRealBaseline {
        /// NTFS volume to measure.
        #[arg(default_value = "C:")]
        drive: String,
    },
    /// Check that the dev environment matches the `mise.toml` pins and the gate
    /// prerequisites (tool versions, lefthook, elevation, the build/ layout).
    Doctor,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Version { channel, date } => version::run(&channel, date.as_deref()),
        Commands::Publish { skip_rust } => publish::run(skip_rust),
        Commands::PublishUiTest { skip_rust } => publish::run_ui_test(skip_rust),
        Commands::Package { tag } => package::run(tag.as_deref()),
        Commands::CheckVersion { tag } => version::check_release_tag(&tag),
        Commands::CleanTemp => {
            clean::run();
            Ok(())
        }
        Commands::SignStage => signing::run_stage(),
        Commands::SignCollect => signing::run_collect(),
        Commands::TestAdmin => test_admin::run(),
        Commands::ContractBless => contract_bless::run(),
        Commands::DocsAssemble => docs::run(),
        Commands::PerfPreflight => perf::run(),
        Commands::PerfMicroCheck => perf::micro_check(),
        Commands::PerfMicroBaseline => perf::micro_baseline(),
        Commands::PerfRealCheck { drive } => perf::real_check(&drive),
        Commands::PerfRealBaseline { drive } => perf::real_baseline(&drive),
        Commands::Doctor => doctor::run(),
    }
}
