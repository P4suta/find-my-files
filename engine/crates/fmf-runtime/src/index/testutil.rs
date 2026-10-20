//! Test fixtures: the RAII test directory and sample-index builders.
//!
//! `feature = "testutil"` exposes this module to the other workspace crates' test suites (dev-dependencies only — production builds never compile it).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{EntryId, Frn, RawEntry, VolumeIndex, VolumeIndexBuilder};
use crate::ondisk::ntfs::ROOT_RECORD;

const SEQUENCE_SHIFT: u32 = u64::BITS - u16::BITS;

/// Publish appended entries into the sort permutations and bump the content generation, for a fixture index.
///
/// `VolumeIndex::merge_new_into_permutations` is crate-internal so that production callers can only reach it through the USN batch path, which decides what a rejected topology means.
/// A fixture that fails that validation is simply a broken fixture, so this panics instead of handing the caller an error it has no way to act on.
///
/// # Panics
///
/// If the fixture's parent graph no longer forms a valid live topology.
pub fn merge_new_into_permutations(index: &mut VolumeIndex, first_new: EntryId) {
    index
        .merge_new_into_permutations(first_new)
        .expect("fixture topology remains valid");
}

/// RAII per-test directory: `{build/engine}/test-tmp/fmf-<pid>-<seq>`,
/// created by [`TestDir::new`], removed (best-effort) on drop.
///
/// Per-call uniqueness matters: the engine's writer lock turns a shared index dir into a cross-test collision under the parallel test runner.
/// The dir lives under the build output tree (`build/engine`) — not %TEMP% —
/// so whatever a killed test run leaves behind is swept by `cargo clean` or `just clean-temp`.
///
/// Hold the guard for the whole test (`let (_dir, e) = …`): it must drop *after* everything that has files open inside it, or the removal quietly fails and the directory outlives the test.
pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    /// Create a fresh per-test directory under the build output tree.
    ///
    /// # Panics
    ///
    /// Panics if the directory cannot be created.
    #[must_use]
    pub fn new() -> Self {
        let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
            || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../build/engine"),
            PathBuf::from,
        );
        Self::new_in(target)
    }

    /// Create a fresh per-test directory below an explicit volume root.
    ///
    /// This is crate-visible for live volume tests whose file references must belong to the same volume as the OS metadata handle under test.
    #[must_use]
    pub(crate) fn new_in(target: impl AsRef<Path>) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = target.as_ref().join("test-tmp").join(format!(
            "fmf-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create test dir");
        Self { path }
    }

    /// The absolute path of this per-test directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve `p` relative to this test directory.
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
        if let Err(error) = std::fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "cannot remove test directory {}: {error}",
                self.path.display()
            );
        }
    }
}

/// Build a [`RawEntry`] from a bare record number.
///
/// Synthesizes its FRN as `(1 << 48) | record` (sequence 1) and clears reparse/hidden/system.
/// `size` is in bytes; `mtime` is a FILETIME (100ns ticks since 1601, UTC).
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
        parent_frn: Frn(if parent == ROOT_RECORD {
            parent
        } else {
            (1u64 << SEQUENCE_SHIFT) | parent
        }),
        frn: Frn((1u64 << SEQUENCE_SHIFT) | record),
        name_utf16: name,
        is_dir,
        is_reparse: false,
        is_hidden: false,
        is_system: false,
        size,
        mtime,
    }
}

/// Encode `s` as a UTF-16 code-unit vector, as scan entry names arrive.
#[must_use]
pub fn u16s(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

struct FixtureEntry {
    record: u64,
    parent: u64,
    name: &'static str,
    directory: bool,
    size: u64,
    mtime: i64,
}

const SAMPLE_ENTRIES: &[FixtureEntry] = &[
    FixtureEntry {
        record: 100,
        parent: 50,
        name: "Note.TXT",
        directory: false,
        size: 10,
        mtime: 300,
    },
    FixtureEntry {
        record: 50,
        parent: 5,
        name: "docs",
        directory: true,
        size: 0,
        mtime: 100,
    },
    FixtureEntry {
        record: 60,
        parent: 5,
        name: "big.bin",
        directory: false,
        size: 99_999,
        mtime: 200,
    },
];
const HARDLINK_ENTRIES: &[FixtureEntry] = &[
    FixtureEntry {
        record: 10,
        parent: 5,
        name: "a",
        directory: true,
        size: 0,
        mtime: 1,
    },
    FixtureEntry {
        record: 20,
        parent: 5,
        name: "b",
        directory: true,
        size: 0,
        mtime: 1,
    },
    FixtureEntry {
        record: 100,
        parent: 10,
        name: "shared.txt",
        directory: false,
        size: 42,
        mtime: 2,
    },
    FixtureEntry {
        record: 100,
        parent: 20,
        name: "alias.txt",
        directory: false,
        size: 42,
        mtime: 2,
    },
];

fn build_fixture(entries: &[FixtureEntry]) -> VolumeIndex {
    let mut builder = VolumeIndexBuilder::new_synthetic("C:", ROOT_RECORD);
    for entry in entries {
        builder.push(raw(
            entry.record,
            entry.parent,
            &u16s(entry.name),
            entry.directory,
            entry.size,
            entry.mtime,
        ));
    }
    builder.finish()
}

/// Build a sample with a child encountered before its parent directory.
#[must_use]
pub fn build_sample() -> VolumeIndex {
    build_fixture(SAMPLE_ENTRIES)
}

/// Build two links to the same file under different directories.
#[must_use]
pub fn build_hardlink_sample() -> VolumeIndex {
    build_fixture(HARDLINK_ENTRIES)
}

/// Build a [`RawEntry`] for attribute-filter tests: like [`raw`] but with explicit `is_hidden`/`is_system` and zeroed `size`/`mtime`.
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
        parent_frn: Frn(if parent == ROOT_RECORD {
            parent
        } else {
            (1u64 << SEQUENCE_SHIFT) | parent
        }),
        frn: Frn((1u64 << SEQUENCE_SHIFT) | record),
        name_utf16: name,
        is_dir,
        is_reparse: false,
        is_hidden,
        is_system,
        size: 0,
        mtime: 0,
    }
}
