//! Structural tripwires for the shipped-release SBOM re-scan
//! (`.github/workflows/sbom-monitor.yml`, ADR-0034).
//!
//! This is the one supply-chain check that looks at bytes users have already
//! downloaded rather than at HEAD, and it went red on 2026-08-03 and stayed red
//! for six consecutive weekly runs. Nobody could say why. The identity and
//! structure assertions were written as `jq -e '...' "$path" >/dev/null`, and
//! `jq -e` reports a false predicate as exit status 1 with the word `false` on
//! the stdout that `>/dev/null` threw away — so `set -e` ended the step with
//! `##[error]Process completed with exit code 1.` and nothing else. A monitor
//! that cannot say what it found is indistinguishable from one nobody reads.
//!
//! These tests pin that defect class, plus the scope of the single identity
//! waiver. They deliberately do not pin message wording or jq filter bodies:
//! the diagnostics should stay freely editable — only their existence, and the
//! narrowness of the waiver, are load-bearing.

const SBOM_MONITOR_WORKFLOW: &str = include_str!("../../.github/workflows/sbom-monitor.yml");

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = text
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start `{start}`"));
    let (section, _) = tail
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end `{end}`"));
    section
}

/// The workflow with whole-line comments stripped — the text that actually
/// runs. The prose in that file is free to quote `jq -e` and to name v0.1.0
/// while explaining why neither may appear in the shell itself.
fn executable_text(workflow: &str) -> String {
    workflow
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_failing_exit_names_what_it_found() {
    let lines: Vec<&str> = SBOM_MONITOR_WORKFLOW.lines().collect();
    let mut guarded = 0_usize;

    for (index, line) in lines.iter().enumerate() {
        if line.trim() != "exit 1" {
            continue;
        }
        guarded += 1;

        // Walk back to the opener of the block this `exit 1` lives in. The
        // `::error::` has to be inside that same block: whoever opens the run
        // log is told which file and which field failed in the same breath as
        // the failure, or the run has degenerated into a bare exit code again.
        let mut announced = false;
        for candidate in lines[..index].iter().rev() {
            let trimmed = candidate.trim();
            if trimmed.contains("::error::") {
                announced = true;
                break;
            }
            if trimmed.ends_with("; then")
                || trimmed == "else"
                || trimmed == "*)"
                || trimmed == "fi"
                || trimmed == "done"
            {
                break;
            }
        }

        assert!(
            announced,
            "sbom-monitor.yml:{} exits 1 without an ::error:: in its own block",
            index + 1
        );
    }

    // An empty sweep is a green gate that proves nothing. If the failing exits
    // are ever restructured out of existence, this guard must be re-aimed
    // rather than left quietly passing over nothing.
    assert!(
        guarded >= 9,
        "expected the monitor to still have its failing exits; found only {guarded}"
    );
}

#[test]
fn shipped_sbom_checks_never_hide_behind_a_jq_exit_code() {
    for (index, line) in SBOM_MONITOR_WORKFLOW.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        assert!(
            !line.contains("jq -e"),
            "sbom-monitor.yml:{}: `jq -e` fails with a bare exit status. Render the \
             actual values and compare them, so the mismatch prints itself.",
            index + 1
        );
    }
}

#[test]
fn identity_waiver_covers_one_file_of_one_immutable_release() {
    let executable = executable_text(SBOM_MONITOR_WORKFLOW);

    let waivers: Vec<&str> = executable
        .lines()
        .filter(|line| line.contains("\"$tag\" = \""))
        .collect();
    assert_eq!(
        waivers.len(),
        1,
        "exactly one release may be exempted by tag equality; found {waivers:?}"
    );

    let waiver = waivers[0];
    assert!(
        waiver.contains("v0.1.1"),
        "the only exempt release is v0.1.1, whose engine SBOM shipped before #164 \
         standardized metadata.component and which is immutable: `{waiver}`"
    );
    assert!(
        waiver.contains("fmf-engine.cdx.json"),
        "v0.1.1's app.cdx.json carries correct identity and must stay strictly \
         checked, so the waiver names the one broken file: `{waiver}`"
    );
    assert!(
        !executable.contains("v0.1.0"),
        "v0.1.0 published zero assets, so the asset-name check rejects it long \
         before identity validation. Naming it in the shell would imply it gets there."
    );
    assert!(
        SBOM_MONITOR_WORKFLOW.contains("v0.2.0"),
        "the waiver must record when it stops applying: v0.2.0 becoming latest"
    );

    // A waived check announces itself. Silently accepting is how a monitor
    // becomes decorative.
    let waived_branch = between(&executable, waiver, "else");
    assert!(
        waived_branch.contains("::notice::"),
        "the waived path must say so in the run log"
    );
    assert!(
        !waived_branch.contains("exit"),
        "the waiver relaxes identity only; it must never take over a failure path"
    );

    // Relaxing identity is the whole of the waiver, and the digest comparison
    // is what keeps that safe: it binds the downloaded bytes to the release
    // metadata GitHub serves. `.components` is the array osv-scanner actually
    // reads. Both therefore run for every release, outside any tag conditional.
    let before_identity = between(&executable, "mkdir -p sbom", "expected_identity=");
    for unconditional in ["sha256sum", "bomFormat", "specVersion"] {
        assert!(
            before_identity.contains(unconditional),
            "`{unconditional}` must be validated ahead of identity, for every release"
        );
    }
    assert!(
        !before_identity.contains("\"$tag\" = \""),
        "digest and structure validation must not be conditioned on the release tag"
    );
}

#[test]
fn both_halves_agree_on_the_canonical_sbom_pair() {
    // Fetching and validating are separate steps (see the comment above the
    // validation step: a >4096-byte bash `run:` body deadlocks actionlint's
    // shellcheck pipe on Windows, hanging `just lint` with no output). That
    // split means the canonical filename pair is spelled twice, so the two
    // halves can disagree — and a validator iterating a shorter list than the
    // downloader would check fewer SBOMs than it downloaded, silently.
    let executable = executable_text(SBOM_MONITOR_WORKFLOW);
    let pair = r#"expected=("app.cdx.json" "fmf-engine.cdx.json")"#;
    assert_eq!(
        executable.matches(pair).count(),
        2,
        "the download step and the validation step must name the same SBOM pair"
    );
}
