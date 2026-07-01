//! `xtask test-admin` — run the elevation-gated `#[ignore]` engine tests
//! (real-volume MFT/USN) with the `FMF_ADMIN_TESTS` gate set.
//!
//! Exists so the just recipe needs no shell-specific env syntax: the gate flag
//! is handed straight to the child `cargo` via [`cmd::run_env`], never through
//! powershell.exe (which mangles `cargo --config 'env.X="1"'` into a bare `1`
//! cargo rejects — the reason the recipe used to hard-code a `$env:` line).

use crate::{cmd, paths};
use anyhow::Result;

pub fn run() -> Result<()> {
    cmd::run_env(
        &paths::engine_dir(),
        "cargo",
        &["test", "--workspace", "--", "--ignored"],
        &[("FMF_ADMIN_TESTS", "1")],
    )
}
