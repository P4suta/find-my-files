//! Test fixtures: the RAII test directory and sample-index builders.
//!
//! `feature = "testutil"` exposes this module to the other workspace
//! crates' test suites (dev-dependencies only — production builds never
//! compile it).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Frn, RawEntry, VolumeIndex, VolumeIndexBuilder};

/// RAII per-test directory: `{workspace target}/test-tmp/fmf-<pid>-<seq>`,
/// created by [`TestDir::new`], removed (best-effort) on drop.
///
/// Per-call uniqueness matters: the engine's writer lock turns a shared
/// index dir into a cross-test collision under the parallel test runner.
/// The dir lives under the workspace `target/` — not %TEMP% — so whatever
/// a killed test run leaves behind is swept by `cargo clean` or
/// `just clean-temp`.
///
/// Hold the guard for the whole test (`let (_dir, e) = …`): it must drop
/// *after* everything that has files open inside it, or the removal
/// quietly fails and the directory outlives the test.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    /// Create a fresh per-test directory under the workspace `target/`.
    ///
    /// # Panics
    ///
    /// Panics if the directory cannot be created.
    #[must_use]
    pub fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
            || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
            PathBuf::from,
        );
        let path = target.join("test-tmp").join(format!(
            "fmf-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, p: impl AsRef<Path>) -> PathBuf {
        self.path.join(p)
    }
}

impl Default for TestDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        // Best-effort by design: a still-open handle (a leaked engine, a
        // child process on its way down) keeps files alive on Windows.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[must_use]
pub const fn raw(
    record: u64,
    parent: u64,
    name: &[u16],
    is_dir: bool,
    size: u64,
    mtime: i64,
) -> RawEntry<'_> {
    RawEntry {
        parent_frn: Frn(parent),
        frn: Frn((1u64 << 48) | record),
        name_utf16: name,
        is_dir,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size,
        mtime,
    }
}

#[must_use]
pub fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// C:\ ├─ docs\ ├─ note.txt   docs comes *after* its child in scan order.
#[must_use]
pub fn build_sample() -> VolumeIndex {
    let mut b = VolumeIndexBuilder::new("C:", 5);
    let note = u16s("Note.TXT");
    let docs = u16s("docs");
    let big = u16s("big.bin");
    b.push(raw(100, 50, &note, false, 10, 300)); // parent not yet pushed
    b.push(raw(50, 5, &docs, true, 0, 100));
    b.push(raw(60, 5, &big, false, 99_999, 200));
    b.finish()
}

#[must_use]
pub const fn raw_attr(
    record: u64,
    parent: u64,
    name: &[u16],
    is_dir: bool,
    is_hidden: bool,
    is_system: bool,
) -> RawEntry<'_> {
    RawEntry {
        parent_frn: Frn(parent),
        frn: Frn((1u64 << 48) | record),
        name_utf16: name,
        is_dir,
        is_reparse: false,
        is_hidden,
        is_system,
        size: 0,
        mtime: 0,
    }
}
