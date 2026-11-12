use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{bail, ensure, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{cmd, mutation::CsharpIdentity, paths};

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Equivalent {
    identity: CsharpIdentity,
    rationale: String,
}

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    schema_version: u32,
    tool: BTreeMap<String, String>,
    examined_files: Vec<String>,
    accepted_equivalents: Vec<Equivalent>,
}

struct Hunk {
    old: u64,
    removed: u64,
    new: u64,
    added: u64,
}

fn hunks(diff: &str) -> Result<Vec<Hunk>> {
    let pattern = Regex::new(
        r"^@@ -(?P<old>\d+)(?:,(?P<removed>\d+))? \+(?P<new>\d+)(?:,(?P<added>\d+))? @@",
    )?;
    diff.lines()
        .filter(|line| line.starts_with("@@ "))
        .map(|line| {
            let values = pattern.captures(line).context("invalid Git hunk header")?;
            Ok(Hunk {
                old: values["old"].parse()?,
                removed: values
                    .name("removed")
                    .map_or(Ok(1), |n| n.as_str().parse())?,
                new: values["new"].parse()?,
                added: values.name("added").map_or(Ok(1), |n| n.as_str().parse())?,
            })
        })
        .collect()
}

fn map_line(line: u64, hunks: &[Hunk]) -> Result<u64> {
    let mut mapped = line;
    for hunk in hunks {
        if hunk.removed == 0 {
            if line > hunk.old {
                mapped = mapped.checked_add(hunk.added).context("line overflow")?;
            }
        } else if line < hunk.old {
            break;
        } else if line
            < hunk
                .old
                .checked_add(hunk.removed)
                .context("hunk overflow")?
        {
            ensure!(
                hunk.removed == hunk.added,
                "mutation line {line} changed; inspect its identity manually"
            );
            return hunk
                .new
                .checked_add(line - hunk.old)
                .context("line overflow");
        } else {
            mapped = mapped
                .checked_sub(hunk.removed)
                .and_then(|line| line.checked_add(hunk.added))
                .context("line overflow")?;
        }
    }
    Ok(mapped)
}

fn span(source: &str, identity: &CsharpIdentity) -> Result<Vec<u16>> {
    ensure!(
        identity.start_line > 0
            && identity.start_column > 0
            && identity.end_column > 0
            && identity.end_line >= identity.start_line,
        "invalid mutation range"
    );
    let mut units = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let number = u64::try_from(index)? + 1;
        if number < identity.start_line || number > identity.end_line {
            continue;
        }
        let line: Vec<u16> = line.encode_utf16().collect();
        let start = if number == identity.start_line {
            usize::try_from(identity.start_column - 1)?
        } else {
            0
        };
        let end = if number == identity.end_line {
            usize::try_from(identity.end_column - 1)?
        } else {
            line.len()
        };
        units.extend_from_slice(
            line.get(start..end)
                .context("mutation columns escape source")?,
        );
        if number != identity.end_line {
            units.push(u16::from(b'\n'));
        }
    }
    ensure!(
        u64::try_from(source.lines().count())? >= identity.end_line && !units.is_empty(),
        "empty mutation source span"
    );
    Ok(units)
}

fn strip_csharp(source: &str) -> Result<String> {
    let normalized = source.replace("\r\n", "\n");
    let mut child = Command::new("ocomment")
        .current_dir(paths::repo_root())
        .args([
            "strip",
            "--language",
            "csharp",
            "--policy",
            "all",
            "--layout",
            "compact",
            "--quiet",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start the pinned OComment lexer")?;
    let mut input = child.stdin.take().context("missing OComment stdin")?;
    let output = std::thread::scope(|scope| -> Result<_> {
        let writer = scope.spawn(move || input.write_all(normalized.as_bytes()));
        let output = child.wait_with_output()?;
        writer
            .join()
            .map_err(|_| anyhow::anyhow!("OComment input writer panicked"))??;
        Ok(output)
    })?;
    ensure!(
        output.status.success(),
        "OComment failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.replace("\r\n", "\n"))
}

pub fn relocate_csharp() -> Result<()> {
    let root = paths::repo_root();
    let path = "app/FindMyFiles.Tests/mutation-baseline.json";
    let original = cmd::capture_stdout(&root, "git", &["show", &format!("HEAD:{path}")])?;
    let mut baseline: Baseline = serde_json::from_str(&original)?;
    let current: Baseline = serde_json::from_slice(&fs::read(root.join(path))?)?;
    let original_policy = baseline == current;
    let mut sources = BTreeMap::new();
    for entry in &mut baseline.accepted_equivalents {
        let source_path = &entry.identity.path;
        ensure!(
            source_path.starts_with("app/FindMyFiles/")
                && !source_path.contains("..")
                && !source_path.contains('\\'),
            "invalid mutation source path"
        );
        if !sources.contains_key(source_path) {
            let old = cmd::capture_stdout(&root, "git", &["show", &format!("HEAD:{source_path}")])?;
            let new = fs::read_to_string(root.join(source_path))?;
            ensure!(
                strip_csharp(&old)? == strip_csharp(&new)?,
                "executable source changed in {source_path}; reassess its mutation policy"
            );
            let diff = cmd::capture_stdout(
                &root,
                "git",
                &[
                    "diff",
                    "--no-ext-diff",
                    "--unified=0",
                    "HEAD",
                    "--",
                    source_path,
                ],
            )?;
            sources.insert(source_path.clone(), (old, new, hunks(&diff)?));
        }
        let Some((old, new, changes)) = sources.get(source_path) else {
            bail!("missing mutation source");
        };
        let original_span = span(old, &entry.identity)?;
        let original_start = entry.identity.start_line;
        entry.identity.start_line = map_line(entry.identity.start_line, changes)?;
        entry.identity.end_line = map_line(entry.identity.end_line, changes)?;
        ensure!(
            original_span == span(new, &entry.identity)?,
            "mutation source changed at {source_path}:{original_start}"
        );
    }
    ensure!(
        original_policy || baseline == current,
        "mutation policy changed beyond relocated source coordinates"
    );
    let mut output = serde_json::to_string_pretty(&baseline)?;
    output.push('\n');
    fs::write(root.join(path), output)?;
    println!("Relocated {} existing identities; source spans, mutation operators, replacements, and rationales remain identical.", baseline.accepted_equivalents.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_only_unchanged_lines_and_rejects_ambiguous_edits() {
        let diff = hunks("@@ -0,0 +1,2 @@\n@@ -3,2 +5,0 @@\n").expect("Git hunks");
        assert_eq!(map_line(1, &diff).expect("unchanged"), 3);
        assert_eq!(map_line(5, &diff).expect("unchanged"), 5);
        assert!(map_line(3, &diff).is_err());
    }

    #[test]
    fn mutation_columns_count_utf16_code_units() {
        let identity = CsharpIdentity {
            path: "fixture.cs".into(),
            start_line: 1,
            start_column: 5,
            end_line: 1,
            end_column: 10,
            mutator: "Boolean mutation".into(),
            replacement: "true".into(),
        };
        assert_eq!(
            span("\"😀\"false", &identity).expect("UTF-16 span"),
            "false".encode_utf16().collect::<Vec<_>>()
        );
    }
}
