//! Repository-ruleset drift detection.
//!
//! `.github/rulesets/*.json` is the reviewable, version-controlled definition of
//! this repository's protection — but GitHub never applies those files. They are
//! disaster-recovery templates a maintainer re-exports by hand after every UI or
//! API change, and nothing proved the re-export actually happened. An
//! un-exported change is invisible in both directions: the tree keeps describing
//! protection the repository does not have, and the repository keeps enforcing
//! rules no reviewed file describes.
//!
//! The check is split into two halves that never mix:
//!
//! * [`run_fetch`] captures the live rulesets through `gh`. This is the only
//!   network call in xtask. It lives here rather than in the `just` recipe
//!   because capturing needs one API call *per ruleset id*: GitHub's list
//!   endpoint returns identity and enforcement only — no `rules`, no
//!   `conditions`, no `bypass_actors` — so the ids must be read first and then
//!   followed one by one. A loop over a value discovered at run time cannot be
//!   written in a `just` recipe without shell-specific syntax, which AGENTS.md
//!   and the `task_definitions_never_depend_on_one_shell` guard both forbid.
//! * [`run_check`] compares the two sides. It is pure, offline and unit-tested:
//!   it reads two directories and reports what differs.
//!
//! Both halves are read-only with respect to GitHub. Applying a template is a
//! maintainer action in the UI; this command only ever says what differs.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{cmd, fsx, paths};

/// Top-level fields the API attaches to describe *its own* bookkeeping rather
/// than the protection a ruleset applies: identity (`id`, `node_id`), history
/// (`created_at`, `updated_at`), navigation (`_links`), where the ruleset is
/// configured (`source`, `source_type`), and what the *calling* token happens to
/// be allowed to bypass (`current_user_can_bypass`). None of them can be
/// expressed in a checked-in template, so comparing them would report drift on
/// every single run and train the reader to ignore the report.
///
/// `bypass_actors` is deliberately **not** in this list: who may bypass a
/// ruleset is part of the protection, so a live bypass actor the template does
/// not declare is drift, not noise.
///
/// This set is policy, not an implementation detail, so a test pins it as a
/// literal: the list cannot grow without a reviewed edit to that test.
const SERVER_ASSIGNED_FIELDS: &[&str] = &[
    "_links",
    "created_at",
    "current_user_can_bypass",
    "id",
    "node_id",
    "source",
    "source_type",
    "updated_at",
];

/// The rulesets REST collection of whichever repository `gh` resolves from its
/// working directory. `{owner}`/`{repo}` are gh's own placeholders, so the
/// repository name is never hard-coded here.
const RULESETS_ENDPOINT: &str = "repos/{owner}/{repo}/rulesets";

/// The one path whose membership difference is the whole point of the required-
/// status-checks rule, so the report spells out what the list means.
const REQUIRED_CHECKS_PATH: &str =
    "rules[required_status_checks].parameters.required_status_checks";

/// Whether a difference is about a plain field or a whole rule. Only the wording
/// differs, and the wording is the entire value of this report.
#[derive(Debug, Clone, Copy)]
enum What {
    Field,
    Rule,
}

impl What {
    const fn noun(self) -> &'static str {
        match self {
            Self::Field => "field",
            Self::Rule => "rule",
        }
    }
}

/// What kind of difference was found at one path inside a ruleset.
#[derive(Debug)]
enum Detail {
    /// Both sides set it, to different values.
    Changed { template: String, live: String },
    /// The template declares it and the live ruleset has nothing there.
    OnlyInTemplate { what: What, value: String },
    /// The live ruleset has it and the template never mentions it — normally a
    /// field GitHub introduced after the template was last exported.
    OnlyInLive { what: What, value: String },
    /// Order-independent membership difference for a list.
    Members {
        missing_from_live: Vec<String>,
        only_in_live: Vec<String>,
    },
}

/// One difference, addressed by a path such as
/// `rules[pull_request].parameters.allowed_merge_methods`.
#[derive(Debug)]
struct Difference {
    path: String,
    detail: Detail,
}

/// One ruleset-level finding.
#[derive(Debug)]
enum Finding {
    /// A committed template with no live ruleset of that name: the protection
    /// the tree describes is not applied to the repository at all.
    NotApplied { name: String, summary: String },
    /// A live ruleset with no committed template: real protection that no
    /// reviewable file describes, and that a disaster-recovery restore from
    /// this tree would silently drop.
    Untracked { name: String, summary: String },
    /// The same name on both sides, with different content.
    Drifted {
        name: String,
        differences: Vec<Difference>,
    },
}

impl Finding {
    #[cfg(test)]
    fn name(&self) -> &str {
        match self {
            Self::NotApplied { name, .. }
            | Self::Untracked { name, .. }
            | Self::Drifted { name, .. } => name,
        }
    }
}

/// The outcome of one comparison.
#[derive(Debug)]
struct Report {
    template_dir: PathBuf,
    live_dir: PathBuf,
    template_count: usize,
    live_count: usize,
    findings: Vec<Finding>,
}

impl Report {
    /// The human-readable report body, printed whether or not there is drift —
    /// a check that says nothing when it passes cannot be trusted when it fails.
    fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "ruleset drift check");
        let _ = writeln!(out, "  templates: {}", self.template_dir.display());
        let _ = writeln!(out, "  live:      {}", self.live_dir.display());
        let _ = writeln!(
            out,
            "  compared:  {} committed template(s) against {} live ruleset(s), by name",
            self.template_count, self.live_count
        );
        let _ = writeln!(out);

        if self.findings.is_empty() {
            let _ = writeln!(out, "in sync: every ruleset matches its template.");
            return out;
        }

        for finding in &self.findings {
            match finding {
                Finding::NotApplied { name, summary } => {
                    let _ = writeln!(out, "NOT APPLIED  {name}");
                    let _ = writeln!(
                        out,
                        "    no live ruleset has this name: the repository does not enforce \
                         the committed template at all"
                    );
                    let _ = writeln!(out, "    template: {summary}");
                }
                Finding::Untracked { name, summary } => {
                    let _ = writeln!(out, "UNTRACKED    {name}");
                    let _ = writeln!(
                        out,
                        "    the repository enforces a ruleset that no committed template \
                         describes"
                    );
                    let _ = writeln!(out, "    live: {summary}");
                }
                Finding::Drifted { name, differences } => {
                    let _ = writeln!(
                        out,
                        "DRIFTED      {name}  ({} difference(s))",
                        differences.len()
                    );
                    for difference in differences {
                        render_difference(&mut out, difference);
                    }
                }
            }
            let _ = writeln!(out);
        }

        let _ = writeln!(
            out,
            "{} ruleset(s) differ from the committed templates.",
            self.findings.len()
        );
        out
    }

    /// The finding recorded for `name`, if any. Tests assert on this rather than
    /// on a position in the list.
    #[cfg(test)]
    fn finding(&self, name: &str) -> Option<&Finding> {
        self.findings.iter().find(|finding| finding.name() == name)
    }
}

fn render_difference(out: &mut String, difference: &Difference) {
    let _ = writeln!(out, "    {}", describe_path(&difference.path));
    match &difference.detail {
        Detail::Changed { template, live } => {
            let _ = writeln!(out, "        template: {template}");
            let _ = writeln!(out, "        live:     {live}");
        }
        Detail::OnlyInTemplate { what, value } => {
            let _ = writeln!(
                out,
                "        missing from live — the live ruleset has no such {} (template: {value})",
                what.noun()
            );
        }
        Detail::OnlyInLive { what, value } => {
            let _ = writeln!(
                out,
                "        live only — the template predates this {} (live: {value})",
                what.noun()
            );
        }
        Detail::Members {
            missing_from_live,
            only_in_live,
        } => {
            if !missing_from_live.is_empty() {
                let _ = writeln!(
                    out,
                    "        missing from live: {}",
                    missing_from_live.join(", ")
                );
            }
            if !only_in_live.is_empty() {
                let _ = writeln!(
                    out,
                    "        live only:         {}",
                    only_in_live.join(", ")
                );
            }
        }
    }
}

/// The required-status-checks list is the one path where "which members differ"
/// is the whole finding, so it gets a sentence instead of only a JSON path.
fn describe_path(path: &str) -> String {
    if path == REQUIRED_CHECKS_PATH {
        format!("{path}  (the contexts that must pass before a merge)")
    } else {
        path.to_owned()
    }
}

/// Capture every live ruleset into `out_dir`, one `<id>.json` per ruleset.
///
/// The directory is replaced, not merged: a ruleset deleted in the UI must not
/// survive as a stale file that makes the next check look clean.
pub fn run_fetch(out_dir: &Path) -> Result<()> {
    let repo = paths::repo_root();
    let listing = gh(&repo, RULESETS_ENDPOINT)?;
    let listing: Value = serde_json::from_str(&listing)
        .context("`gh api` returned a ruleset listing that is not JSON")?;
    let listing = listing
        .as_array()
        .ok_or_else(|| anyhow!("the ruleset listing is not a JSON array"))?;

    let mut ids = Vec::with_capacity(listing.len());
    for entry in listing {
        let id = entry
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("a listed ruleset has no numeric `id`: {entry}"))?;
        ids.push(id);
    }

    fsx::force_remove_dir_all(out_dir)
        .with_context(|| format!("failed to clear {}", out_dir.display()))?;
    fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    for id in &ids {
        let body = gh(&repo, &format!("{RULESETS_ENDPOINT}/{id}"))?;
        let body: Value = serde_json::from_str(&body)
            .with_context(|| format!("`gh api` returned non-JSON for ruleset {id}"))?;
        let mut pretty = serde_json::to_string_pretty(&body)
            .with_context(|| format!("failed to re-serialize ruleset {id}"))?;
        pretty.push('\n');
        let path = out_dir.join(format!("{id}.json"));
        fsx::write_file_atomic(&path, pretty.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
    }

    println!(
        "captured {} live ruleset(s) into {}",
        ids.len(),
        out_dir.display()
    );
    Ok(())
}

/// One read-only `gh api` GET. Every ruleset request in this module goes through
/// here, so the module can be audited for writes by reading this one function:
/// no `--method`, no fields, nothing but a GET.
fn gh(dir: &Path, endpoint: &str) -> Result<String> {
    cmd::capture_stdout(dir, "gh", &["api", endpoint]).with_context(|| {
        format!(
            "failed to read {endpoint} — `gh` must be installed and authenticated with a \
             token that has admin access to this repository"
        )
    })
}

/// Compare the committed templates with a captured live snapshot, print the
/// report, and fail when anything differs.
pub fn run_check(live_dir: &Path) -> Result<()> {
    let report = compare(&paths::ruleset_templates_dir(), live_dir)?;
    print!("{}", report.render());
    if report.findings.is_empty() {
        return Ok(());
    }
    bail!(
        "ruleset drift: {} ruleset(s) do not match the committed templates (see the report \
         above). Either re-export the live ruleset into .github/rulesets/, or apply the \
         template in the repository settings — both are maintainer actions, never automated.",
        report.findings.len()
    );
}

/// The whole comparison: pure, offline, and the only thing the tests exercise.
fn compare(template_dir: &Path, live_dir: &Path) -> Result<Report> {
    let templates = load_directory(template_dir, "template")?;
    let live = load_directory(live_dir, "live")?;

    let names: BTreeSet<&String> = templates.keys().chain(live.keys()).collect();
    let mut findings = Vec::new();
    for name in names {
        match (templates.get(name), live.get(name)) {
            (Some(template), Some(live)) => {
                let differences = diff_rulesets(template, live)?;
                if !differences.is_empty() {
                    findings.push(Finding::Drifted {
                        name: name.clone(),
                        differences,
                    });
                }
            }
            (Some(template), None) => findings.push(Finding::NotApplied {
                name: name.clone(),
                summary: summarize(template),
            }),
            (None, Some(live)) => findings.push(Finding::Untracked {
                name: name.clone(),
                summary: summarize(live),
            }),
            // Unreachable: `name` came out of one of the two maps.
            (None, None) => {}
        }
    }

    Ok(Report {
        template_dir: template_dir.to_path_buf(),
        live_dir: live_dir.to_path_buf(),
        template_count: templates.len(),
        live_count: live.len(),
        findings,
    })
}

/// Load every `*.json` in `dir`, keyed by the `name` each one declares.
///
/// Non-JSON files are skipped by extension, which is what keeps the templates'
/// own `README.md` out of the comparison. An empty directory is an error: a
/// check that reads nothing reports no drift, and a silent pass is exactly the
/// failure this command exists to prevent (a failed `gh` fetch would otherwise
/// read as "every template is unapplied").
fn load_directory(dir: &Path, side: &str) -> Result<BTreeMap<String, Value>> {
    let entries = fs::read_dir(dir)
        .with_context(|| format!("cannot read the {side} ruleset directory {}", dir.display()))?;

    let mut files: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot list {}", dir.display()))?;
        let path = entry.path();
        let is_json = path
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if is_json && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            files.push(path);
        }
    }
    files.sort();

    if files.is_empty() {
        bail!(
            "no *.json file in the {side} ruleset directory {} — refusing to compare against \
             an empty side, because that would report success no matter what the other side \
             holds",
            dir.display()
        );
    }

    let mut rulesets = BTreeMap::new();
    for path in files {
        let text =
            fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?;
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("{} has no string `name`", path.display()))?
            .to_owned();
        if rulesets.insert(name.clone(), normalize(value)?).is_some() {
            bail!(
                "two {side} files declare the ruleset name `{name}` ({}); a ruleset name is \
                 the only thing the two sides are matched on, so it has to be unique",
                dir.display()
            );
        }
    }
    Ok(rulesets)
}

/// Drop the [`SERVER_ASSIGNED_FIELDS`] from the top level of one ruleset.
///
/// Top level only, on purpose: a rule parameter that happens to be called
/// `source` is part of the protection and must still be compared.
fn normalize(mut value: Value) -> Result<Value> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("a ruleset file must hold a JSON object"))?;
    for field in SERVER_ASSIGNED_FIELDS {
        object.remove(*field);
    }
    Ok(value)
}

/// A one-line description of a ruleset, for the sides that have no counterpart
/// to diff against.
fn summarize(body: &Value) -> String {
    let target = body
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or("(no target)");
    let enforcement = body
        .get("enforcement")
        .and_then(Value::as_str)
        .unwrap_or("(no enforcement)");
    let rules = body
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| rule.get("type").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    format!("target {target}, enforcement {enforcement}, rules: {rules}")
}

fn diff_rulesets(template: &Value, live: &Value) -> Result<Vec<Difference>> {
    let template = as_object(template, "template")?;
    let live = as_object(live, "live")?;
    let mut differences = Vec::new();
    diff_objects("", template, live, &mut differences)?;
    differences.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(differences)
}

fn as_object<'a>(value: &'a Value, side: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| anyhow!("a {side} ruleset must be a JSON object, got {value}"))
}

fn diff_objects(
    path: &str,
    template: &Map<String, Value>,
    live: &Map<String, Value>,
    out: &mut Vec<Difference>,
) -> Result<()> {
    let keys: BTreeSet<&String> = template.keys().chain(live.keys()).collect();
    for key in keys {
        let child = join_path(path, key);
        match (template.get(key), live.get(key)) {
            (Some(template_value), Some(live_value)) => {
                match (
                    path.is_empty() && key == "rules",
                    template_value,
                    live_value,
                ) {
                    (true, Value::Array(template_rules), Value::Array(live_rules)) => {
                        diff_rules(template_rules, live_rules, out)?;
                    }
                    _ => diff_values(&child, template_value, live_value, out)?,
                }
            }
            (Some(template_value), None) => out.push(Difference {
                path: child,
                detail: Detail::OnlyInTemplate {
                    what: What::Field,
                    value: display(template_value),
                },
            }),
            (None, Some(live_value)) => out.push(Difference {
                path: child,
                detail: Detail::OnlyInLive {
                    what: What::Field,
                    value: display(live_value),
                },
            }),
            // Unreachable: `key` came out of one of the two maps.
            (None, None) => {}
        }
    }
    Ok(())
}

fn diff_values(
    path: &str,
    template: &Value,
    live: &Value,
    out: &mut Vec<Difference>,
) -> Result<()> {
    match (template, live) {
        (Value::Object(template), Value::Object(live)) => diff_objects(path, template, live, out),
        (Value::Array(template), Value::Array(live)) => {
            diff_lists(path, template, live, out);
            Ok(())
        }
        _ => {
            if template != live {
                out.push(Difference {
                    path: path.to_owned(),
                    detail: Detail::Changed {
                        template: display(template),
                        live: display(live),
                    },
                });
            }
            Ok(())
        }
    }
}

/// `rules` is an array whose order carries no meaning — GitHub returns the rules
/// in whatever order it likes — so it is compared as a map keyed by rule type.
/// That also makes every nested difference addressable as
/// `rules[<type>].parameters.<field>` instead of `rules[3]`, which would move
/// the moment a rule is added.
fn diff_rules(template: &[Value], live: &[Value], out: &mut Vec<Difference>) -> Result<()> {
    let template = rules_by_type(template, "template")?;
    let live = rules_by_type(live, "live")?;
    let types: BTreeSet<&String> = template.keys().chain(live.keys()).collect();
    for rule_type in types {
        let path = format!("rules[{rule_type}]");
        match (template.get(rule_type), live.get(rule_type)) {
            (Some(template_rule), Some(live_rule)) => {
                diff_values(&path, template_rule, live_rule, out)?;
            }
            (Some(template_rule), None) => out.push(Difference {
                path,
                detail: Detail::OnlyInTemplate {
                    what: What::Rule,
                    value: canonical(template_rule),
                },
            }),
            (None, Some(live_rule)) => out.push(Difference {
                path,
                detail: Detail::OnlyInLive {
                    what: What::Rule,
                    value: canonical(live_rule),
                },
            }),
            // Unreachable: `rule_type` came out of one of the two maps.
            (None, None) => {}
        }
    }
    Ok(())
}

fn rules_by_type<'a>(rules: &'a [Value], side: &str) -> Result<BTreeMap<String, &'a Value>> {
    let mut by_type = BTreeMap::new();
    for rule in rules {
        let rule_type = rule
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("a {side} rule has no string `type`: {rule}"))?;
        if by_type.insert(rule_type.to_owned(), rule).is_some() {
            bail!(
                "the {side} ruleset declares the rule type `{rule_type}` twice; rules are \
                 matched by type, so a duplicate has no single meaning"
            );
        }
    }
    Ok(by_type)
}

/// Lists are compared as multisets: a status-check context or a merge method
/// means the same thing wherever it sits in the array, and GitHub does not
/// promise an order.
fn diff_lists(path: &str, template: &[Value], live: &[Value], out: &mut Vec<Difference>) {
    let template = members(template);
    let live = members(live);
    let missing_from_live = surplus(&template, &live);
    let only_in_live = surplus(&live, &template);
    if missing_from_live.is_empty() && only_in_live.is_empty() {
        return;
    }
    out.push(Difference {
        path: path.to_owned(),
        detail: Detail::Members {
            missing_from_live,
            only_in_live,
        },
    });
}

/// One list element, in both the form that decides equality and the form a
/// human reads.
struct Member {
    canonical: String,
    display: String,
}

fn members(values: &[Value]) -> Vec<Member> {
    let mut members: Vec<Member> = values
        .iter()
        .map(|value| Member {
            canonical: canonical(value),
            display: display(value),
        })
        .collect();
    members.sort_by(|left, right| left.canonical.cmp(&right.canonical));
    members
}

/// The members of `mine` that `theirs` does not also contain, counting
/// duplicates — so a template that lists a context twice against a live ruleset
/// that lists it once still reports a difference.
fn surplus(mine: &[Member], theirs: &[Member]) -> Vec<String> {
    let mut remaining: BTreeMap<&str, usize> = BTreeMap::new();
    for member in theirs {
        *remaining.entry(member.canonical.as_str()).or_insert(0) += 1;
    }
    let mut out = Vec::new();
    for member in mine {
        match remaining.get_mut(member.canonical.as_str()) {
            Some(count) if *count > 0 => *count -= 1,
            _ => out.push(member.display.clone()),
        }
    }
    out
}

/// The form that decides equality. `serde_json`'s map is a `BTreeMap` here (the
/// `preserve_order` feature is off), so this is key-sorted and therefore stable.
fn canonical(value: &Value) -> String {
    value.to_string()
}

/// The form a human reads: bare strings without their JSON quotes, and a
/// single-key `{"context": "..."}` as just the context, because a required
/// status check reads as its context and nothing else.
fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(fields) => fields
            .get("context")
            .and_then(Value::as_str)
            .filter(|_| fields.len() == 1)
            .map_or_else(|| value.to_string(), ToOwned::to_owned),
        _ => value.to_string(),
    }
}

fn join_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The captured pair under `xtask/tests/fixtures/rulesets`: one real export
    /// of this repository's live rulesets and the committed templates as they
    /// stood beside it. It is a *snapshot*, deliberately not a mirror of
    /// `.github/rulesets` — these tests pin what the detector does with a known
    /// input, and must not start failing because a maintainer later fixed one
    /// side or the other.
    fn fixture(side: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("rulesets")
            .join(side)
    }

    fn captured() -> Report {
        compare(&fixture("tree"), &fixture("live")).expect("the captured fixtures must load")
    }

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("xtask-rulesets-{tag}-{}", std::process::id()))
    }

    /// Build a pair of directories from `(filename, body)` lists.
    fn dirs(tag: &str, templates: &[(&str, &str)], live: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let base = scratch(tag);
        let _ = fsx::force_remove_dir_all(&base);
        let template_dir = base.join("tree");
        let live_dir = base.join("live");
        for (dir, files) in [(&template_dir, templates), (&live_dir, live)] {
            fs::create_dir_all(dir).unwrap();
            for (name, body) in files {
                fs::write(dir.join(name), body).unwrap();
            }
        }
        (template_dir, live_dir)
    }

    #[test]
    fn server_assigned_fields_are_exactly_the_reviewed_set() {
        // What the comparison ignores is policy, not an implementation detail:
        // anything added here stops being protected by the check, so growing the
        // list has to be a reviewed edit to this test as well.
        assert_eq!(
            SERVER_ASSIGNED_FIELDS,
            [
                "_links",
                "created_at",
                "current_user_can_bypass",
                "id",
                "node_id",
                "source",
                "source_type",
                "updated_at",
            ]
        );
        assert!(
            !SERVER_ASSIGNED_FIELDS.contains(&"bypass_actors"),
            "who may bypass a ruleset is part of the protection, never noise"
        );
    }

    #[test]
    fn an_identical_pair_is_in_sync() {
        let report = captured();
        assert!(
            report.finding("require-signed-commits").is_none(),
            "the captured require-signed-commits pair is identical once the \
             server-assigned fields are dropped, but the report says:\n{}",
            report.render()
        );
    }

    #[test]
    fn a_template_that_is_not_applied_at_all_is_reported() {
        // The heaviest failure mode: the tree says shipped version tags cannot be
        // moved or deleted, and no live ruleset enforces it.
        let report = captured();
        let finding = report.finding("protect-version-tags");
        assert!(
            matches!(finding, Some(Finding::NotApplied { .. })),
            "a template with no live ruleset of that name must be reported as not applied"
        );
        let rendered = report.render();
        assert!(rendered.contains("NOT APPLIED  protect-version-tags"));
        assert!(
            rendered.contains("does not enforce"),
            "the report must say what being unapplied means:\n{rendered}"
        );
    }

    #[test]
    fn a_live_ruleset_with_no_template_is_reported() {
        let (template_dir, live_dir) = dirs(
            "untracked",
            &[(
                "a.json",
                r#"{"name":"a","target":"branch","rules":[{"type":"deletion"}]}"#,
            )],
            &[
                (
                    "1.json",
                    r#"{"id":1,"name":"a","target":"branch","rules":[{"type":"deletion"}]}"#,
                ),
                (
                    "2.json",
                    r#"{"id":2,"name":"surprise","target":"tag","enforcement":"active","rules":[{"type":"update"}]}"#,
                ),
            ],
        );
        let report = compare(&template_dir, &live_dir).unwrap();
        assert!(matches!(
            report.finding("surprise"),
            Some(Finding::Untracked { .. })
        ));
        let rendered = report.render();
        assert!(rendered.contains("UNTRACKED    surprise"), "{rendered}");
        assert!(rendered.contains("target tag"), "{rendered}");
    }

    #[test]
    fn every_required_check_missing_from_live_is_named() {
        // The drift that actually matters: three required contexts the template
        // declares are not required by the live ruleset, so they cannot block a
        // merge. Naming them is the whole point of the report.
        let rendered = captured().render();
        let checks = rendered
            .lines()
            .skip_while(|line| !line.contains(REQUIRED_CHECKS_PATH))
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        assert!(
            checks.contains("missing from live:"),
            "the required-check difference must list the missing contexts:\n{rendered}"
        );
        for context in ["release-gate", "audit", "analyze-actions"] {
            assert!(
                checks.contains(context),
                "`{context}` does not gate merges on the live ruleset and must be named:\n\
                 {rendered}"
            );
        }
        assert!(
            rendered.contains("the contexts that must pass before a merge"),
            "the required-check path must say what the list is:\n{rendered}"
        );
    }

    #[test]
    fn differences_are_reported_per_rule_type() {
        let rendered = captured().render();
        assert!(
            rendered.contains("rules[pull_request].parameters.dismiss_stale_reviews_on_push"),
            "a changed rule parameter must be addressed by rule type:\n{rendered}"
        );
        assert!(
            rendered.contains("rules[pull_request].parameters.allowed_merge_methods"),
            "a merge-method difference must be reported:\n{rendered}"
        );
        assert!(
            rendered.contains("DRIFTED      protect-default-branch"),
            "{rendered}"
        );
    }

    #[test]
    fn a_field_only_github_sets_is_reported_as_predating_the_template() {
        let rendered = captured().render();
        assert!(
            rendered.contains(
                "rules[pull_request].parameters.require_extra_approval_for_unattributed_changes"
            ),
            "a live-only field must be reported, not silently ignored:\n{rendered}"
        );
        assert!(
            rendered.contains("live only — the template predates this field"),
            "a live-only field must read as a stale template, not as an unknown \
             failure:\n{rendered}"
        );
    }

    #[test]
    fn only_server_assigned_fields_differing_is_in_sync() {
        // Non-vacuity of the normalization: both bodies carry every ignored
        // field, with different values, and nothing else differs.
        let (template_dir, live_dir) = dirs(
            "normalized",
            &[(
                "a.json",
                r#"{"id":1,"node_id":"AAA","created_at":"2020-01-01","updated_at":"2020-01-02",
                    "_links":{"self":{"href":"https://example.invalid/1"}},
                    "source":"owner/one","source_type":"Repository",
                    "current_user_can_bypass":"always",
                    "name":"a","target":"branch","enforcement":"active",
                    "bypass_actors":[],"rules":[{"type":"deletion"}]}"#,
            )],
            &[(
                "999.json",
                r#"{"id":999,"node_id":"ZZZ","created_at":"2099-12-31","updated_at":"2099-12-31",
                    "_links":{"self":{"href":"https://example.invalid/999"}},
                    "source":"owner/other","source_type":"Organization",
                    "current_user_can_bypass":"never",
                    "name":"a","target":"branch","enforcement":"active",
                    "bypass_actors":[],"rules":[{"type":"deletion"}]}"#,
            )],
        );
        let report = compare(&template_dir, &live_dir).unwrap();
        assert!(
            report.findings.is_empty(),
            "only the ignored fields differ, so this must be in sync:\n{}",
            report.render()
        );
    }

    #[test]
    fn a_bypass_actor_only_in_live_is_reported() {
        // bypass_actors is empty on both sides of the captured fixtures, so
        // without this the "never drop bypass_actors" rule would pass vacuously.
        let (template_dir, live_dir) = dirs(
            "bypass",
            &[(
                "a.json",
                r#"{"name":"a","target":"branch","bypass_actors":[],"rules":[{"type":"deletion"}]}"#,
            )],
            &[(
                "1.json",
                r#"{"id":1,"name":"a","target":"branch","bypass_actors":[
                    {"actor_id":5,"actor_type":"RepositoryRole","bypass_mode":"always"}],
                    "rules":[{"type":"deletion"}]}"#,
            )],
        );
        let report = compare(&template_dir, &live_dir).unwrap();
        assert!(matches!(report.finding("a"), Some(Finding::Drifted { .. })));
        let rendered = report.render();
        assert!(rendered.contains("bypass_actors"), "{rendered}");
        assert!(rendered.contains("RepositoryRole"), "{rendered}");
    }

    #[test]
    fn rule_order_does_not_affect_the_comparison() {
        let (template_dir, live_dir) = dirs(
            "order",
            &[(
                "a.json",
                r#"{"name":"a","target":"branch","rules":[
                    {"type":"deletion"},{"type":"non_fast_forward"},
                    {"type":"required_status_checks","parameters":{"required_status_checks":[
                        {"context":"one"},{"context":"two"}]}}]}"#,
            )],
            &[(
                "1.json",
                r#"{"id":1,"name":"a","target":"branch","rules":[
                    {"type":"required_status_checks","parameters":{"required_status_checks":[
                        {"context":"two"},{"context":"one"}]}},
                    {"type":"non_fast_forward"},{"type":"deletion"}]}"#,
            )],
        );
        let report = compare(&template_dir, &live_dir).unwrap();
        assert!(
            report.findings.is_empty(),
            "rules and check contexts are unordered sets:\n{}",
            report.render()
        );
    }

    #[test]
    fn an_empty_template_directory_is_an_error() {
        let (template_dir, live_dir) = dirs(
            "empty-tree",
            &[],
            &[("1.json", r#"{"id":1,"name":"a","target":"branch"}"#)],
        );
        let error = compare(&template_dir, &live_dir).unwrap_err().to_string();
        assert!(error.contains("no *.json file"), "{error}");
        assert!(error.contains("template"), "{error}");
    }

    #[test]
    fn an_empty_live_directory_is_an_error() {
        // A failed capture must not read as "every template is unapplied".
        let (template_dir, live_dir) = dirs(
            "empty-live",
            &[("a.json", r#"{"name":"a","target":"branch"}"#)],
            &[],
        );
        let error = compare(&template_dir, &live_dir).unwrap_err().to_string();
        assert!(error.contains("no *.json file"), "{error}");
        assert!(error.contains("live"), "{error}");
    }

    #[test]
    fn two_files_declaring_one_name_are_rejected() {
        let (template_dir, live_dir) = dirs(
            "duplicate",
            &[
                ("a.json", r#"{"name":"a","target":"branch"}"#),
                ("b.json", r#"{"name":"a","target":"tag"}"#),
            ],
            &[("1.json", r#"{"id":1,"name":"a","target":"branch"}"#)],
        );
        let error = compare(&template_dir, &live_dir).unwrap_err().to_string();
        assert!(error.contains("declare the ruleset name"), "{error}");
    }

    #[test]
    fn a_duplicate_rule_type_is_rejected() {
        let (template_dir, live_dir) = dirs(
            "duplicate-rule",
            &[(
                "a.json",
                r#"{"name":"a","rules":[{"type":"deletion"},{"type":"deletion"}]}"#,
            )],
            &[(
                "1.json",
                r#"{"id":1,"name":"a","rules":[{"type":"deletion"}]}"#,
            )],
        );
        let error = compare(&template_dir, &live_dir).unwrap_err().to_string();
        assert!(error.contains("twice"), "{error}");
    }

    #[test]
    fn the_committed_templates_all_load() {
        // Content-independent: this must keep passing whichever way the drift is
        // resolved. It pins that the real directory is readable, that the
        // templates parse and declare unique names, and that README.md beside
        // them is skipped rather than treated as a malformed ruleset.
        let templates = load_directory(&paths::ruleset_templates_dir(), "template")
            .expect("the committed ruleset templates must load");
        assert!(
            !templates.is_empty(),
            ".github/rulesets must hold at least one template"
        );
    }
}
