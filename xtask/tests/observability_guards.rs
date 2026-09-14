//! Executable tripwires for the observability plumbing — the instrumentation
//! that exists but whose output does not arrive.
//!
//! Every failure guarded here is silent by construction: a log line that
//! renders its own body as `[redacted]`, a fault token that reproduces on one
//! engine and not the other, two halves of one privacy policy drifting into
//! two policies. None of them fail a build, none raise an error, and each one
//! is discovered only by a developer who needed the missing output and did not
//! get it. These tests read the committed tree as text, in the same structural
//! style as the guards beside them.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const DIAG: &str = include_str!("../../engine/crates/fmf-core/src/diag.rs");
const SERVICE_FAULTS: &str = include_str!("../../engine/crates/fmf-service/src/faults.rs");
const FAKE_ENGINE: &str = include_str!("../../app/FindMyFiles/Engine/FakeEngineClient.cs");
const SANITIZER: &str = include_str!("../../app/FindMyFiles/Services/DiagnosticSanitizer.cs");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent")
        .to_path_buf()
}

/// Every committed `.rs` file, by directory scan rather than through the git
/// index, so an untracked stray is seen too. `build/`, `target/` and `.git/`
/// are generated or not source, and neither is another checkout that happens
/// to sit inside this one: a `git worktree` placed under the repository (they
/// land in `.claude/worktrees/` here) holds *another branch's* sources, so
/// scanning it makes this guard report defects that were fixed on main and
/// pass or fail depending on what else the machine has checked out. CI never
/// has one, so the disagreement only ever appears locally.
fn rust_sources() -> Vec<(String, String)> {
    let root = repo();
    let mut found = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", directory.display()));
        for entry in entries {
            let entry = entry.expect("directory entry must be readable");
            let kind = entry.file_type().expect("file type must be readable");
            let path = entry.path();
            if kind.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                // A nested checkout carries its own `.git` (a file for a
                // worktree, a directory for a clone); that probe is the general
                // rule, and `.claude` is named as well so the common case costs
                // no syscall.
                let nested_checkout = path.join(".git").exists();
                if !nested_checkout
                    && !matches!(name.as_str(), ".git" | ".claude" | "build" | "target")
                {
                    pending.push(path);
                }
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
                let name = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .display()
                    .to_string()
                    .replace('\\', "/");
                found.push((name, text));
            }
        }
    }
    found.sort();
    found
}

/// The `!!` query texts a fault-injecting engine must recognize. This list is
/// the reviewed one: the guard below holds both implementations against *it*,
/// rather than against each other, so the two cannot agree on a set that is
/// missing a token neither side ever implemented.
const FAULT_TOKENS: &[&str] = &["!!drop", "!!lag", "!!panic", "!!warn"];

/// The `"!!<token>"` string literals in `source` — deliberately only quoted
/// ones, so a token that a doc comment merely *mentions* cannot satisfy the
/// parity check on behalf of code that never handles it.
fn fault_token_literals(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (index, _) in source.match_indices("\"!!") {
        let rest = &source[index + 1..];
        let end = rest
            .char_indices()
            .find(|(_, ch)| !matches!(ch, '!' | 'a'..='z'))
            .map_or(rest.len(), |(offset, _)| offset);
        if rest[end..].starts_with('"') && end > 2 {
            found.insert(rest[..end].to_string());
        }
    }
    found
}

#[test]
fn no_tracing_macro_records_the_message_as_a_field_named_msg() {
    // The logfmt formatter renders an event's message as ` msg=`, which makes
    // `msg = "…"` look like the right way to spell it. It is not: that is a
    // *field* named `msg`, and the formatter's fail-closed allowlist does not
    // carry it, so the body is replaced by a redaction marker and the line
    // says nothing. Allowlisting `msg` would not rescue it either — message
    // prose contains spaces and fails safe_diagnostic_tag as well.
    //
    // Fourteen call sites across the index builder and the volume scan were
    // logging into that hole, which is exactly the cost of a mistake nothing
    // reports: the code looks instrumented and is not. The positional form
    // (`tracing::debug!(area = "index", "finish: …")`) is the one that reaches
    // the reader.
    // This file states the offending spellings as literals in order to look
    // for them, so it is the one file that must not be scanned. Excluded by
    // exact path, and the exclusion is asserted to have matched, so a rename
    // cannot quietly widen it.
    const SELF: &str = "xtask/tests/observability_guards.rs";

    let sources = rust_sources();
    let mut violations = Vec::new();
    let mut instrumented = 0usize;
    let mut skipped_self = 0usize;
    for (name, text) in &sources {
        instrumented += text.matches("tracing::").count();
        if name == SELF {
            skipped_self += 1;
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // The multi-line macro-argument form, then the inline forms. A
            // `let msg = …` binding matches none of them.
            if trimmed.starts_with("msg = ")
                || line.contains(", msg = ")
                || line.contains("(msg = ")
            {
                violations.push(format!("{name}:{}: {}", number + 1, trimmed));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "a tracing event's message is a positional argument, not a `msg` field \
         (the formatter emits it as msg= itself); these lines log their body as \
         a redaction marker: {violations:#?}"
    );

    // Non-vacuity: a walk that found no Rust, or Rust that uses no tracing,
    // would make the assertion above pass while checking nothing.
    assert!(
        sources.len() > 100,
        "the source walk collapsed to {} .rs files; the guard would be vacuous",
        sources.len()
    );
    assert_eq!(
        skipped_self, 1,
        "exactly this guard is exempt; `{SELF}` was skipped {skipped_self} time(s)"
    );
    assert!(
        instrumented > 0,
        "no `tracing::` call was scanned at all; this guard is pinned to the \
         tracing macros and must move if they do"
    );
}

#[test]
fn redaction_says_which_of_the_two_causes_fired() {
    // A bare `[redacted]` reports that something was dropped but not why, and
    // the two causes need opposite fixes: an unreviewed field name is fixed by
    // extending the allowlist, an unsafe value by correcting the call site.
    // Readers who could not tell them apart chased the wrong one.
    for marker in ["[redacted:unknown-field]", "[redacted:unsafe-value]"] {
        assert!(
            DIAG.contains(marker),
            "fmf-core's formatter must emit {marker}"
        );
        assert!(
            SANITIZER.contains(marker),
            "the C# diagnostics sanitizer must emit {marker} — the same policy \
             stated the same way, so one bug report reads consistently"
        );
    }

    // And the ambiguous marker must not survive in either implementation.
    // (FileLog's app.log tail keeps a plain `[redacted]`: it withholds every
    // message unconditionally, so there is only ever one cause to report.)
    for (name, source) in [("diag.rs", DIAG), ("DiagnosticSanitizer.cs", SANITIZER)] {
        assert!(
            !source.contains("\"[redacted]\""),
            "{name} still emits the cause-less `[redacted]` marker"
        );
    }
}

#[test]
fn the_diagnostics_allowlists_stay_fail_closed() {
    // Both halves redact by default and name what may pass. A refactor that
    // inverted either default — passing unknown fields and naming the unsafe
    // ones — would leak silently and pass every other test in the tree, since
    // nothing else asserts on a field nobody has added yet.
    assert!(
        DIAG.contains("_ => push_field(&mut self.fields, name, REDACTED_UNKNOWN_FIELD)"),
        "the formatter's catch-all arm must redact, not pass through"
    );
    assert!(
        SANITIZER.contains("if (!Allowlist.TryGetValue(path, out var shape)")
            && SANITIZER.contains("return RedactedUnknownField;"),
        "the sanitizer must redact anything its allowlist does not name"
    );
}

#[test]
fn fault_injection_tokens_match_on_both_engines() {
    // The fake engine exists so a developer can reproduce a failure without
    // installing a service. A token only one side implements defeats that:
    // `!!drop` was missing from the fake, and a dropped response is precisely
    // the shape of failure that looks like a hang — the case least reachable
    // any other way. `!!warn` was missing from the service, so the warning
    // pipeline could only ever be exercised against a double.
    let service = fault_token_literals(SERVICE_FAULTS);
    let fake = fault_token_literals(FAKE_ENGINE);
    let reviewed: BTreeSet<String> = FAULT_TOKENS.iter().map(|t| (*t).to_string()).collect();

    // Non-vacuity first: an extractor that silently matched nothing would make
    // both comparisons below trivially true against an empty reviewed list.
    assert!(
        !reviewed.is_empty() && !service.is_empty() && !fake.is_empty(),
        "no fault tokens were extracted (reviewed {reviewed:?}, service {service:?}, fake {fake:?})"
    );

    assert_eq!(
        service, reviewed,
        "fmf-service's faults.rs must handle exactly the reviewed fault tokens"
    );
    assert_eq!(
        fake, reviewed,
        "FakeEngineClient must handle exactly the reviewed fault tokens — a \
         fault that reproduces on only one engine is a trap, not a tool"
    );
}
