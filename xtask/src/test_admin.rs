//! `xtask test-admin` — run the elevation-gated `#[ignore]` engine tests
//! (real-volume MFT/USN and machine-security boundaries) with the
//! `FMF_ADMIN_TESTS` gate set.
//!
//! Exists so the just recipe needs no shell-specific env syntax: the gate flag
//! is handed straight to the child `cargo` via [`cmd::run_env`], never through
//! powershell.exe (which mangles `cargo --config 'env.X="1"'` into a bare `1`
//! cargo rejects — the reason the recipe used to hard-code a `$env:` line).
//!
//! The inventory below is the machine-checked census of that privileged
//! surface. It is pinned from two independent directions, because a gate that
//! reports green while executing nothing is worse than no gate at all:
//!
//! * the non-elevated tripwires in this file's test module compare it against
//!   the working tree, so a test that is added, renamed, moved or deleted
//!   without updating the inventory fails on an ordinary `just test-xtask`;
//! * [`require_admin_test_inventory`] compares it against the real
//!   cargo-nextest listing before an elevated run starts, so a test that no
//!   longer compiles into the suite can never be reported as covered.

use std::collections::BTreeSet;

use crate::{cmd, paths};
use anyhow::{bail, Context, Result};

/// One elevation-gated `#[ignore]` engine test.
struct AdminTest {
    /// Repository-relative, forward-slashed file that declares it.
    source: &'static str,
    /// cargo-nextest binary id of the suite that carries it.
    binary_id: &'static str,
    /// Test name inside that suite, module path included.
    name: &'static str,
}

/// The complete census of elevation-gated tests in the engine workspace.
///
/// `#[ignore]` in this repository means exactly one thing — "needs an elevated
/// Administrator terminal" (CLAUDE.md / AGENTS.md) — so this list and the set
/// of ignored tests must be identical, not merely overlapping. The equality is
/// enforced in both directions rather than "every required test exists":
/// a one-way check cannot notice the failure mode that actually happened here,
/// namely a privileged surface growing to fifteen tests while the gate still
/// looked at one.
const ADMIN_TESTS: &[AdminTest] = &[
    AdminTest {
        source: "engine/crates/fmf-core/src/engine/tests.rs",
        binary_id: "fmf-core",
        name: "engine::tests::engine_e2e_scan_query_snapshot_restore",
    },
    AdminTest {
        source: "engine/crates/fmf-core/src/scan/mod.rs",
        binary_id: "fmf-core",
        name: "scan::tests::streaming_scan_matches_live_exact_records",
    },
    AdminTest {
        source: "engine/crates/fmf-core/src/usn/session.rs",
        binary_id: "fmf-core",
        name: "usn::session::tests::live_metadata_returns_the_complete_hard_link_set",
    },
    AdminTest {
        source: "engine/crates/fmf-core/src/usn/session.rs",
        binary_id: "fmf-core",
        name: "usn::session::tests::usn_journal_live_open_query_and_one_read",
    },
    AdminTest {
        source: "engine/crates/fmf-core/src/usn/session.rs",
        binary_id: "fmf-core",
        name: "usn::session::tests::usn_quiet_journal_read_returns_bounded",
    },
    // docs/SECURITY.md declares this one the required machine-security gate:
    // real Windows tokens, a real second local user, three pipe boundaries.
    AdminTest {
        source: "engine/crates/fmf-service/src/pipe.rs",
        binary_id: "fmf-service",
        name: "pipe::admin_security_tests::named_pipe_security_boundaries_are_enforced_on_real_tokens_and_transports",
    },
    // Registers a real Scheduled Task: the only way to prove the GC task
    // document is one the registrar accepts. A string assertion over the same
    // XML stayed green for an element `schtasks` rejects outright.
    AdminTest {
        source: "engine/crates/fmf-service/tests/lifecycle_admin.rs",
        binary_id: "fmf-service::lifecycle_admin",
        name: "gc_task_xml_registers_with_schtasks",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "hard_links_are_rejected_before_acl_or_content_mutation",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "managed_tree_depth_is_bounded_without_recursive_stack_growth",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "preopened_delete_child_handle_blocks_root_adoption",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "preopened_delete_handle_blocks_root_adoption",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "preopened_write_dac_handle_is_rotated_out_of_the_privileged_name",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "preopened_write_owner_handle_is_rotated_out_of_the_privileged_name",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "provenance_match_never_repairs_a_drifted_root_acl_in_place",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/security_admin.rs",
        binary_id: "fmf-service::security_admin",
        name: "reparse_and_preopened_mutation_handles_fail_closed",
    },
    AdminTest {
        source: "engine/crates/fmf-service/tests/service_admin.rs",
        binary_id: "fmf-service::service_admin",
        name: "service_e2e_flush_survives_kill_and_restores",
    },
];

#[cfg(windows)]
fn require_elevated_process() -> Result<()> {
    use std::{ffi::c_void, mem::size_of, ptr};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = ptr::null_mut();
    // SAFETY: `token` is a valid out pointer; the returned handle is closed on
    // every path after a successful call.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(std::io::Error::last_os_error()).context("query current process token");
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0;
    // SAFETY: `elevation` is writable for the declared size and `token` is a
    // live process-token handle returned above.
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast::<c_void>(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &raw mut returned,
        )
    };
    let query_error = (queried == 0).then(std::io::Error::last_os_error);
    // SAFETY: `token` is owned by this function and is closed exactly once.
    let closed = unsafe { CloseHandle(token) };
    if let Some(error) = query_error {
        return Err(error).context("read process elevation");
    }
    if closed == 0 {
        return Err(std::io::Error::last_os_error()).context("close process token");
    }
    if returned != size_of::<TOKEN_ELEVATION>() as u32 {
        bail!("Windows returned an unexpected TOKEN_ELEVATION size ({returned})");
    }
    if elevation.TokenIsElevated == 0 {
        bail!(
            "`just test-admin` requires an elevated Administrator terminal; \
             no admin tests were started"
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn require_elevated_process() -> Result<()> {
    bail!("`just test-admin` is Windows-only")
}

fn require_admin_test_inventory() -> Result<()> {
    let listing = cmd::capture(
        &paths::engine_dir(),
        "cargo",
        &[
            "nextest",
            "list",
            "--locked",
            "--workspace",
            "--profile",
            "admin",
            "--message-format",
            "json",
        ],
    )
    .context("failed to enumerate the admin-test inventory with cargo-nextest")?;
    require_admin_test_inventory_in(&listing)
}

/// Every `(binary id, test name)` cargo-nextest reports as ignored.
///
/// A missing or renamed schema key is reported as a schema error, never as an
/// absent test: the two demand opposite responses (fix the parser vs. restore
/// the test), so they must never share an error path. The key is `testcases`,
/// with no hyphen — verified against `cargo-nextest 0.9.140`, the version
/// pinned by `engine/.config/nextest.toml` and the CI install steps.
fn ignored_tests_in(listing: &str) -> Result<BTreeSet<(String, String)>> {
    let listing: serde_json::Value =
        serde_json::from_str(listing).context("parse cargo-nextest admin-test inventory")?;
    let suites = listing
        .get("rust-suites")
        .and_then(serde_json::Value::as_object)
        .context("cargo-nextest inventory has no rust-suites object")?;
    let mut ignored = BTreeSet::new();
    for (binary_id, suite) in suites {
        let cases = suite
            .get("testcases")
            .and_then(serde_json::Value::as_object)
            .with_context(|| {
                format!(
                    "cargo-nextest suite `{binary_id}` has no testcases object — the `nextest \
                     list --message-format json` schema changed; fix this parser rather than \
                     trusting its verdict"
                )
            })?;
        for (name, case) in cases {
            if case.get("ignored").and_then(serde_json::Value::as_bool) == Some(true) {
                ignored.insert((binary_id.clone(), name.clone()));
            }
        }
    }
    Ok(ignored)
}

fn registered_tests() -> BTreeSet<(String, String)> {
    ADMIN_TESTS
        .iter()
        .map(|test| (test.binary_id.to_owned(), test.name.to_owned()))
        .collect()
}

/// Renders a difference for a human who now has to act on it, so each line
/// also carries the file the inventory expects to find the test in.
fn describe(tests: &BTreeSet<(String, String)>) -> String {
    use std::fmt::Write as _;

    tests
        .iter()
        .fold(String::new(), |mut rendered, (binary_id, name)| {
            let source = ADMIN_TESTS
                .iter()
                .find(|test| test.binary_id == binary_id && test.name == name)
                .map_or("not registered in ADMIN_TESTS", |test| test.source);
            let _ = write!(rendered, "\n  {binary_id} {name}  ({source})");
            rendered
        })
}

fn require_admin_test_inventory_in(listing: &str) -> Result<()> {
    let compiled = ignored_tests_in(listing)?;
    let registered = registered_tests();

    let missing: BTreeSet<(String, String)> = registered.difference(&compiled).cloned().collect();
    let unregistered: BTreeSet<(String, String)> =
        compiled.difference(&registered).cloned().collect();
    if !missing.is_empty() {
        bail!(
            "required admin security tests are missing from the compiled inventory:{}{}",
            describe(&missing),
            if unregistered.is_empty() {
                String::new()
            } else {
                format!(
                    "\nand these ignored tests are not registered:{}",
                    describe(&unregistered)
                )
            }
        );
    }
    if !unregistered.is_empty() {
        bail!(
            "these ignored tests are not registered in ADMIN_TESTS, so `just test-admin` would \
             run privileged code this gate does not account for:{}",
            describe(&unregistered)
        );
    }
    Ok(())
}

pub fn run() -> Result<()> {
    require_elevated_process()?;
    require_admin_test_inventory()?;
    cmd::run_env(
        &paths::engine_dir(),
        "cargo",
        &[
            "nextest",
            "run",
            "--locked",
            "--workspace",
            "--profile",
            "admin",
            "--run-ignored",
            "ignored-only",
        ],
        &[("FMF_ADMIN_TESTS", "1")],
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use super::{ignored_tests_in, require_admin_test_inventory_in, ADMIN_TESTS};

    /// The bare `fn` name — `name` without its module path.
    fn leaf(name: &str) -> &str {
        name.rsplit("::").next().unwrap_or(name)
    }

    /// A cargo-nextest listing, reduced from real `cargo nextest list
    /// --message-format json` output (0.9.140) to two suites. It is checked in
    /// verbatim in shape — key spellings included — because the previous
    /// fixture was invented rather than captured, and the invented key
    /// (`test-cases`) meant the preflight could never see any test at all.
    const LISTING: &str = r#"{
      "rust-build-meta": {},
      "test-count": 3,
      "rust-suites": {
        "fmf-service": {
          "binary-id": "fmf-service",
          "kind": "lib",
          "status": "listed",
          "testcases": {
            "pipe::tests::header_roundtrip": { "kind": "test", "ignored": false },
            "pipe::admin_security_tests::named_pipe_security_boundaries_are_enforced_on_real_tokens_and_transports": {
              "kind": "test",
              "ignored": true,
              "filter-match": { "status": "mismatch", "reason": "ignored" }
            }
          }
        },
        "fmf-service::security_admin": {
          "binary-id": "fmf-service::security_admin",
          "kind": "test",
          "status": "listed",
          "testcases": {
            "hard_links_are_rejected_before_acl_or_content_mutation": {
              "kind": "test",
              "ignored": true,
              "filter-match": { "status": "mismatch", "reason": "ignored" }
            }
          }
        }
      }
    }"#;

    #[test]
    fn ignored_tests_are_extracted_from_the_real_nextest_schema() {
        let found = ignored_tests_in(LISTING).expect("the captured schema parses");
        assert_eq!(
            found,
            BTreeSet::from([
                (
                    "fmf-service".to_owned(),
                    "pipe::admin_security_tests::named_pipe_security_boundaries_are_enforced_on_real_tokens_and_transports".to_owned()
                ),
                (
                    "fmf-service::security_admin".to_owned(),
                    "hard_links_are_rejected_before_acl_or_content_mutation".to_owned()
                ),
            ]),
            "only ignored tests count, and they are keyed by suite"
        );
    }

    /// The defect this whole gate existed to prevent: the parser looked for a
    /// key cargo-nextest does not emit, so *no* test was ever found. That must
    /// read as a broken parser, not as a deleted security test.
    #[test]
    fn a_renamed_schema_key_is_a_schema_error_not_a_missing_test() {
        let renamed = LISTING.replace("\"testcases\"", "\"test-cases\"");
        let error = ignored_tests_in(&renamed).expect_err("an unknown schema must fail closed");
        let message = format!("{error:#}");
        assert!(message.contains("schema changed"), "{message}");
    }

    /// A synthetic listing in the captured schema carrying exactly `tests`.
    fn listing_from(tests: &[(String, String)]) -> String {
        let mut by_suite: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (binary_id, name) in tests {
            by_suite
                .entry(binary_id.as_str())
                .or_default()
                .push(name.as_str());
        }
        let suites: Vec<String> = by_suite
            .iter()
            .map(|(binary_id, names)| {
                let cases: Vec<String> = names
                    .iter()
                    .map(|name| format!("\"{name}\": {{\"kind\":\"test\",\"ignored\":true}}"))
                    .collect();
                format!(
                    "\"{binary_id}\": {{\"testcases\": {{{}}}}}",
                    cases.join(",")
                )
            })
            .collect();
        format!("{{\"rust-suites\": {{{}}}}}", suites.join(","))
    }

    #[test]
    fn inventory_drift_fails_in_both_directions() {
        let registered: Vec<(String, String)> = ADMIN_TESTS
            .iter()
            .map(|test| (test.binary_id.to_owned(), test.name.to_owned()))
            .collect();
        require_admin_test_inventory_in(&listing_from(&registered))
            .expect("the exact inventory is accepted");

        let mut deleted = registered.clone();
        deleted.retain(|(_, name)| !name.contains("named_pipe_security_boundaries"));
        assert_eq!(deleted.len(), registered.len() - 1, "fixture sanity");
        let error = require_admin_test_inventory_in(&listing_from(&deleted))
            .expect_err("a deleted security test must fail closed");
        assert!(
            format!("{error:#}").contains("named_pipe_security_boundaries"),
            "{error:#}"
        );

        let mut added = registered;
        added.push((
            "fmf-core".to_owned(),
            "newly_added_privileged_test".to_owned(),
        ));
        let error = require_admin_test_inventory_in(&listing_from(&added))
            .expect_err("an unregistered ignored test must fail closed");
        assert!(
            format!("{error:#}").contains("newly_added_privileged_test"),
            "{error:#}"
        );
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask/ always has a parent (the repo root)")
            .to_path_buf()
    }

    /// Every `.rs` file under `engine/`, as `(repo-relative forward-slashed
    /// path, contents)`. Read from the working tree rather than the git index
    /// so an untracked new test file is caught too.
    fn engine_sources() -> Vec<(String, String)> {
        let root = repo_root();
        let mut sources = Vec::new();
        let mut pending = vec![root.join("engine")];
        while let Some(directory) = pending.pop() {
            let entries = std::fs::read_dir(&directory).unwrap_or_else(|error| {
                panic!("{} must be readable: {error}", directory.display())
            });
            for entry in entries {
                let entry = entry.expect("directory entry must be readable");
                let kind = entry.file_type().expect("file type must be readable");
                let path = entry.path();
                if kind.is_dir() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !matches!(name.as_str(), ".git" | "build" | "target") {
                        pending.push(path);
                    }
                } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
                    let relative = path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    let contents = std::fs::read_to_string(&path)
                        .unwrap_or_else(|error| panic!("{relative} must be readable: {error}"));
                    sources.push((relative, contents));
                }
            }
        }
        sources
    }

    /// Line index of the `fn` signature carrying the `#[ignore]` on `line`.
    fn signature_line(path: &str, lines: &[&str], attribute: usize) -> usize {
        let mut index = attribute + 1;
        while lines
            .get(index)
            .is_some_and(|line| line.trim_start().starts_with('#'))
        {
            index += 1;
        }
        assert!(
            lines
                .get(index)
                .is_some_and(|line| line.trim_start().starts_with("fn ")),
            "{path}:{}: #[ignore] must sit directly on a test fn",
            attribute + 1
        );
        index
    }

    /// The body of the item whose signature is on `signature`, i.e. everything
    /// up to the closing brace at the signature's own indentation. `cargo fmt
    /// --check` is a required gate, so that brace is always alone on its line.
    fn body(lines: &[&str], signature: usize) -> String {
        let line = lines[signature];
        let indent = line.len() - line.trim_start().len();
        let mut collected = String::new();
        for line in &lines[signature + 1..] {
            let trimmed = line.trim();
            if trimmed == "}" && line.len() - line.trim_start().len() == indent {
                break;
            }
            collected.push_str(trimmed);
            collected.push('\n');
        }
        collected
    }

    /// `(source, fn name)` for every `#[ignore]` test declared under `engine/`.
    fn declared_admin_tests() -> BTreeSet<(String, String)> {
        let mut declared = BTreeSet::new();
        for (path, contents) in engine_sources() {
            let lines: Vec<&str> = contents.lines().collect();
            for attribute in 0..lines.len() {
                if !lines[attribute].trim_start().starts_with("#[ignore") {
                    continue;
                }
                let signature = signature_line(&path, &lines, attribute);
                let name = lines[signature]
                    .trim_start()
                    .trim_start_matches("fn ")
                    .split(['(', '<'])
                    .next()
                    .expect("split always yields a first field")
                    .trim();
                declared.insert((path.clone(), name.to_owned()));
            }
        }
        declared
    }

    /// The census that closes the hole this module exists for: the privileged
    /// surface is whatever the tree declares, so the inventory has to equal it.
    /// Non-elevated on purpose — it reads source text and never runs a single
    /// admin test, so every ordinary `just test-xtask` re-proves it.
    #[test]
    fn the_inventory_lists_exactly_the_ignored_tests_in_the_tree() {
        let declared = declared_admin_tests();
        assert!(
            declared.len() > 10,
            "the source walk found almost nothing ({}) — it stopped working, \
             which would make this census vacuous",
            declared.len()
        );
        let registered: BTreeSet<(String, String)> = ADMIN_TESTS
            .iter()
            .map(|test| (test.source.to_owned(), leaf(test.name).to_owned()))
            .collect();
        let unregistered: Vec<&(String, String)> = declared.difference(&registered).collect();
        let stale: Vec<&(String, String)> = registered.difference(&declared).collect();
        assert!(
            unregistered.is_empty(),
            "these elevation-gated tests are not in ADMIN_TESTS, so `just test-admin` reports \
             on a privileged surface it does not know about — add them: {unregistered:#?}"
        );
        assert!(
            stale.is_empty(),
            "ADMIN_TESTS pins tests that no longer exist at that location: {stale:#?}"
        );
    }

    /// The second half of the vacuous-pass defect: a body that returns early
    /// when `FMF_ADMIN_TESTS` is unset is indistinguishable from a body that
    /// proved the boundary. `#[ignore]` is what *skips* these tests; reaching
    /// the body without the arming variable means the harness was invoked
    /// wrong, and the only honest answer to that is a failure.
    #[test]
    fn every_admin_test_fails_closed_when_the_gate_is_unarmed() {
        let mut checked = 0;
        for (path, contents) in engine_sources() {
            let lines: Vec<&str> = contents.lines().collect();
            let mut declares_admin_test = false;
            for attribute in 0..lines.len() {
                if !lines[attribute].trim_start().starts_with("#[ignore") {
                    continue;
                }
                declares_admin_test = true;
                let signature = signature_line(&path, &lines, attribute);
                assert!(
                    body(&lines, signature).contains("require_admin_gate()"),
                    "{path}:{} must call require_admin_gate() — an elevation-gated test that \
                     merely returns when FMF_ADMIN_TESTS is unset passes vacuously",
                    signature + 1
                );
                checked += 1;
            }
            if !declares_admin_test {
                continue;
            }
            let gate = lines
                .iter()
                .position(|line| line.trim_start().starts_with("fn require_admin_gate()"))
                .unwrap_or_else(|| panic!("{path} must define fn require_admin_gate()"));
            let gate = body(&lines, gate);
            assert!(
                gate.contains("assert_eq!") && gate.contains("FMF_ADMIN_TESTS"),
                "{path}: require_admin_gate() must assert on FMF_ADMIN_TESTS, not branch on it"
            );
        }
        assert_eq!(
            checked,
            ADMIN_TESTS.len(),
            "the walk must reach every registered admin test"
        );
    }
}
