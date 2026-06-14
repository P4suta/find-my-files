//! `xtask clean-temp` — sweep leftover `TestDir` fixtures (build/engine/test-tmp).
//! Their Drop-time removal is best-effort, so a killed test run can leave dirs
//! behind; this is the cheaper broom than `cargo clean`.

use crate::{fsx, paths};

pub fn run() {
    let tmp = paths::build_root().join("engine").join("test-tmp");
    // Best-effort, like the old `-ErrorAction SilentlyContinue; exit 0`, but it
    // says so instead of swallowing silently.
    match fsx::force_remove_dir_all(&tmp) {
        Ok(()) => println!("clean-temp: swept {}", tmp.display()),
        Err(e) => eprintln!("warning: could not fully sweep {} ({e})", tmp.display()),
    }
}
