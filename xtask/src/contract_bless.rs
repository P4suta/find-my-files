//! `xtask contract-bless` — explicitly recapture every shared contract golden
//! with `FMF_BLESS=1`, using nextest for both suites.
//!
//! The environment flag is set directly on each cargo child so the ritual is
//! shell-independent and nextest receives it at test runtime.

use crate::{cmd, paths};
use anyhow::Result;

pub fn run() -> Result<()> {
    let engine = paths::engine_dir();
    let env = [("FMF_BLESS", "1")];
    cmd::run_env(
        &engine,
        "cargo",
        &[
            "nextest",
            "run",
            "--locked",
            "-p",
            "fmf-proto",
            "--test",
            "golden",
        ],
        &env,
    )?;
    cmd::run_env(
        &engine,
        "cargo",
        &[
            "nextest",
            "run",
            "--locked",
            "-p",
            "fmf-core",
            "--test",
            "golden_json",
        ],
        &env,
    )
}
