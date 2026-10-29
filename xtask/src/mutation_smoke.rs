use super::{
    expected_tool, find_unique_file, parse_csharp_run, read_baseline, read_stryker_scope,
    run_required, stryker_args, validate_csharp_conclusive, validate_stryker_exit,
    verify_stryker_manifest_pin, write_json_atomic, AcceptedBaseline, CsharpEvidence,
    CsharpIdentity, ToolPin, STRYKER_NAME, STRYKER_VERSION,
};
use crate::{checksum, fsx, mutation_ci, paths, perf};
use anyhow::{ensure, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = "app/FindMyFiles/Engine/EngineErrorSeverityWire.cs";
const EVIDENCE_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Mode {
    Plan,
    Run,
}

#[derive(Debug, Serialize)]
struct Plan {
    tool: ToolPin,
    full_campaign: bool,
    source: String,
    output: PathBuf,
    configuration: Value,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct SourceIdentity {
    git_commit: String,
    dirty: bool,
    dirty_sha256: String,
}

impl SourceIdentity {
    fn capture(repo: &Path) -> Result<Self> {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .context("read activation source commit")?;
        ensure!(
            output.status.success(),
            "read activation source commit failed"
        );
        let git_commit = std::str::from_utf8(&output.stdout)
            .context("activation source commit is not UTF-8")?
            .trim()
            .to_owned();
        let (dirty, dirty_sha256) = perf::repository_fingerprint(repo)?;
        Ok(Self {
            git_commit,
            dirty,
            dirty_sha256,
        })
    }
}

#[derive(Serialize)]
struct Evidence<'a> {
    full_campaign: bool,
    source_before: &'a SourceIdentity,
    source_after: Option<&'a SourceIdentity>,
    source_error: Option<&'a str>,
    configuration: &'a Value,
    report_sha256: String,
    killed: &'a [CsharpIdentity],
    run: CsharpEvidence<'a>,
    passed: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ActivationCounts {
    pub(super) killed: usize,
    pub(super) survived: usize,
    pub(super) no_coverage: usize,
    pub(super) timeouts: usize,
    pub(super) ignored: usize,
    pub(super) outside_scope: usize,
}

impl ActivationCounts {
    pub(super) const fn verified(self) -> bool {
        self.killed > 0
            && self.survived == 0
            && self.no_coverage == 0
            && self.timeouts == 0
            && self.ignored == 0
            && self.outside_scope == 0
    }
}

fn plan() -> Result<Plan> {
    verify_stryker_manifest_pin()?;
    let repo = paths::repo_root();
    let baseline: AcceptedBaseline<CsharpIdentity> = read_baseline(
        &paths::csharp_mutation_baseline(),
        STRYKER_NAME,
        STRYKER_VERSION,
    )?;
    let reviewed = read_stryker_scope(
        &paths::csharp_stryker_config(),
        &repo,
        &baseline.examined_files,
    )?;
    ensure!(
        reviewed.contains(SOURCE),
        "activation source is not reviewed"
    );
    ensure!(
        !baseline
            .accepted_equivalents
            .iter()
            .any(|accepted| accepted.identity.path == SOURCE),
        "activation source has accepted equivalent survivors"
    );
    Ok(Plan {
        tool: expected_tool(STRYKER_NAME, STRYKER_VERSION),
        full_campaign: false,
        source: SOURCE.to_owned(),
        output: paths::csharp_mutation_smoke_dir(),
        configuration: mutation_ci::stryker_config(&[SOURCE.to_owned()])?,
    })
}

pub fn run(mode: Mode) -> Result<()> {
    let plan = plan()?;
    if mode == Mode::Plan {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }
    ensure!(cfg!(windows), "C# activation requires Windows");
    let repo = paths::repo_root();
    let before = SourceIdentity::capture(&repo)?;
    fsx::force_remove_dir_all(&plan.output).context("remove stale activation evidence")?;
    fs::create_dir_all(&plan.output).context("create activation evidence directory")?;
    let config = plan.output.join("stryker-config.json");
    write_json_atomic(&config, &plan.configuration)?;
    let test_dir = paths::csharp_test_dir(&repo);
    run_required(
        Command::new("dotnet")
            .args(["tool", "restore", "--tool-manifest"])
            .arg(repo.join(".config").join("dotnet-tools.json"))
            .current_dir(&test_dir),
        "restore the pinned Stryker activation tool",
    )?;
    let output_arg = plan.output.to_string_lossy().into_owned();
    let config_arg = config.to_string_lossy().into_owned();
    let status = Command::new("dotnet")
        .args(stryker_args(&output_arg, Some(&config_arg)))
        .env("RestoreLockedMode", "true")
        .env("SkipRustBuild", "true")
        .envs(super::csharp_test_profile())
        .current_dir(&test_dir)
        .status()
        .context("run pinned Stryker activation")?;
    let report = find_unique_file(&plan.output, "mutation-report.json")
        .with_context(|| format!("find activation report after Stryker exit {status}"))?;
    let parsed = parse_csharp_run(&report, &repo, &BTreeSet::from([plan.source.clone()]))
        .with_context(|| format!("parse activation report after Stryker exit {status}"))?;
    let after = SourceIdentity::capture(&repo);
    let source_error = after.as_ref().err().map(ToString::to_string);
    let counts = ActivationCounts {
        killed: parsed.killed.len(),
        survived: parsed.survivors.len(),
        no_coverage: parsed.no_coverage.len(),
        timeouts: parsed.timeouts.len(),
        ignored: parsed.ignored.len(),
        outside_scope: parsed.outside_scope_violations.len(),
    };
    let verified = status.success()
        && counts.verified()
        && after.as_ref().is_ok_and(|identity| identity == &before);
    let evidence = plan.output.join("smoke.json");
    write_json_atomic(
        &evidence,
        &Evidence {
            full_campaign: false,
            source_before: &before,
            source_after: after.as_ref().ok(),
            source_error: source_error.as_deref(),
            configuration: &plan.configuration,
            report_sha256: checksum::sha256_hex(&fs::read(&report)?),
            killed: &parsed.killed,
            run: CsharpEvidence {
                schema_version: EVIDENCE_SCHEMA,
                tool: plan.tool,
                baseline_passed: status.success(),
                process_exit_code: status.code(),
                examined_files: &parsed.examined_files,
                survivors: &parsed.survivors,
                no_coverage: &parsed.no_coverage,
                timeouts: &parsed.timeouts,
                ignored: &parsed.ignored,
                redundant_ignored: &parsed.redundant_ignored,
                outside_scope: &parsed.outside_scope,
                outside_scope_violations: &parsed.outside_scope_violations,
            },
            passed: verified,
        },
    )?;
    if let Err(primary) = validate_stryker_exit(status) {
        if let Some(secondary) = source_error {
            return Err(primary.context(format!("source recheck also failed: {secondary}")));
        }
        return Err(primary);
    }
    ensure!(after? == before, "source changed during Stryker activation");
    validate_csharp_conclusive(&parsed, &evidence)?;
    ensure!(counts.verified(), "activation is inconclusive: {counts:?}");
    println!(
        "Stryker activation passed: {} killed mutant(s); full campaign remains unrun ({}).",
        parsed.killed.len(),
        evidence.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{plan, ActivationCounts, SOURCE};

    #[test]
    fn killed_activation_excludes_compile_rejections() {
        let scratch =
            crate::paths::mutation_dir().join(format!("activation-parser-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).expect("create activation fixture directory");
        let path = scratch.join("mutation-report.json");
        let mutant = |line, status| {
            serde_json::json!({
                "mutatorName": "Boolean mutation",
                "replacement": "false",
                "status": status,
                "location": {
                    "start": {"line": line, "column": 4},
                    "end": {"line": line, "column": 8}
                }
            })
        };
        let repo = crate::paths::repo_root();
        let mut report = serde_json::json!({
            "schemaVersion": "2",
            "projectRoot": repo.join("app").join("FindMyFiles"),
            "files": {
                "Engine/EngineErrorSeverityWire.cs": {
                    "mutants": [mutant(10, "Killed"), mutant(11, "CompileError")]
                }
            }
        });
        let scope = std::collections::BTreeSet::from([SOURCE.to_owned()]);
        super::write_json_atomic(&path, &report).expect("write activation fixture");
        let run = super::parse_csharp_run(&path, &repo, &scope).expect("parse mixed outcomes");
        assert_eq!(run.killed.len(), 1);
        report["files"]["Engine/EngineErrorSeverityWire.cs"]["mutants"][0]["status"] =
            serde_json::json!("CompileError");
        super::write_json_atomic(&path, &report).expect("write rejected mutants fixture");
        let run = super::parse_csharp_run(&path, &repo, &scope).expect("parse compile rejections");
        assert_eq!(run.killed, []);
        crate::fsx::force_remove_dir_all(&scratch).expect("remove activation fixtures");
    }

    #[test]
    fn activation_uses_one_reviewed_source_and_preserves_every_other_setting() {
        let plan = plan().expect("activation plan");
        assert!(!plan.full_campaign);
        assert_eq!(plan.source, SOURCE);
        let mut expected: serde_json::Value =
            crate::mutation::read_json(&crate::paths::csharp_stryker_config())
                .expect("reviewed configuration");
        expected["stryker-config"]["mutate"] =
            serde_json::json!(["**/Engine/EngineErrorSeverityWire.cs"]);
        assert_eq!(plan.configuration, expected);
        assert_ne!(plan.output, crate::paths::csharp_mutation_dir());
    }

    #[test]
    fn activation_requires_a_killed_mutant_and_no_unresolved_result() {
        assert!(!ActivationCounts::default().verified());
        let complete = ActivationCounts {
            killed: 1,
            ..ActivationCounts::default()
        };
        assert!(complete.verified());
        for incomplete in [
            ActivationCounts {
                survived: 1,
                ..complete
            },
            ActivationCounts {
                no_coverage: 1,
                ..complete
            },
            ActivationCounts {
                timeouts: 1,
                ..complete
            },
            ActivationCounts {
                ignored: 1,
                ..complete
            },
            ActivationCounts {
                outside_scope: 1,
                ..complete
            },
        ] {
            assert!(!incomplete.verified(), "accepted {incomplete:?}");
        }
    }
}
