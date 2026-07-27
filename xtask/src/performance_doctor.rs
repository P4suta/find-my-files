//! Audit the live GitHub boundary around the privileged performance instrument.
//!
//! This is deliberately separate from the local development doctor: the current
//! user-owned repository cannot create organization runner groups, so release
//! performance stays fail-closed until an organization owner provisions and
//! audits the exact policy described here.

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::process::Command;

use crate::paths;

const GROUP: &str = "fmf-performance";
const CONTROLLER: &str = ".github/workflows/performance-controller.yml@refs/heads/main";
const ENVIRONMENT: &str = "performance";
const EPHEMERAL_LABEL: &str = "fmf-jit-ephemeral";

fn gh_json(args: &[String]) -> Result<Value> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(paths::repo_root())
        .output()
        .context("failed to spawn `gh` (install it and authenticate as an organization owner)")?;
    if !output.status.success() {
        bail!(
            "`gh {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("GitHub CLI returned invalid JSON")
}

fn paged_items<'a>(value: &'a Value, field: &str) -> Result<Vec<&'a Value>> {
    let pages = value
        .as_array()
        .map_or_else(|| vec![value], |pages| pages.iter().collect());
    let mut items = Vec::new();
    for page in pages {
        let page_items = page
            .get(field)
            .and_then(Value::as_array)
            .with_context(|| format!("GitHub response has no `{field}` array"))?;
        items.extend(page_items);
    }
    Ok(items)
}

fn validate(
    repository: &Value,
    groups_response: &Value,
    runners_response: &Value,
    environment: &Value,
    secrets: &Value,
) -> Result<()> {
    let full_name = repository
        .get("full_name")
        .and_then(Value::as_str)
        .context("repository response has no full_name")?;
    if repository.pointer("/owner/type").and_then(Value::as_str) != Some("Organization") {
        bail!(
            "{full_name} is not organization-owned; the restricted performance \
             runner group cannot be provisioned"
        );
    }

    let groups = paged_items(groups_response, "runner_groups")?;
    let matching_groups: Vec<_> = groups
        .into_iter()
        .filter(|group| group.get("name").and_then(Value::as_str) == Some(GROUP))
        .collect();
    if matching_groups.len() != 1 {
        bail!(
            "expected exactly one visible `{GROUP}` runner group, found {}",
            matching_groups.len()
        );
    }
    let group = matching_groups[0];
    if group.get("visibility").and_then(Value::as_str) != Some("selected") {
        bail!("{GROUP} must be restricted to selected repositories");
    }
    if group
        .get("restricted_to_workflows")
        .and_then(Value::as_bool)
        != Some(true)
    {
        bail!("{GROUP} must set restricted_to_workflows=true");
    }
    let expected_workflow = format!("{full_name}/{CONTROLLER}");
    let selected_workflows = group
        .get("selected_workflows")
        .and_then(Value::as_array)
        .context("runner group response has no selected_workflows")?
        .iter()
        .map(|workflow| {
            workflow
                .as_str()
                .context("runner group selected_workflow is not a string")
                .map(str::to_owned)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if selected_workflows != BTreeSet::from([expected_workflow.clone()]) {
        bail!("{GROUP} must allow exactly `{expected_workflow}`, got {selected_workflows:?}");
    }
    if repository.get("private").and_then(Value::as_bool) == Some(false)
        && group
            .get("allows_public_repositories")
            .and_then(Value::as_bool)
            != Some(true)
    {
        bail!("{GROUP} cannot serve this public repository");
    }

    let runners = paged_items(runners_response, "runners")?;
    if runners.len() > 1 {
        bail!(
            "{GROUP} may expose at most the one currently busy JIT runner, found {}",
            runners.len()
        );
    }
    for runner in runners {
        validate_active_jit_runner(runner)?;
    }

    if environment.get("name").and_then(Value::as_str) != Some(ENVIRONMENT) {
        bail!("GitHub response is not the `{ENVIRONMENT}` environment");
    }
    if environment
        .get("can_admins_bypass")
        .and_then(Value::as_bool)
        != Some(false)
    {
        bail!("{ENVIRONMENT} must disable administrator bypass");
    }
    let deployment = environment
        .get("deployment_branch_policy")
        .context("performance environment has no deployment branch policy")?;
    if deployment
        .get("protected_branches")
        .and_then(Value::as_bool)
        != Some(true)
        || deployment
            .get("custom_branch_policies")
            .and_then(Value::as_bool)
            != Some(false)
    {
        bail!("{ENVIRONMENT} must allow protected branches only");
    }
    let reviewer_rules: Vec<_> = environment
        .get("protection_rules")
        .and_then(Value::as_array)
        .context("performance environment has no protection rules")?
        .iter()
        .filter(|rule| rule.get("type").and_then(Value::as_str) == Some("required_reviewers"))
        .collect();
    if reviewer_rules.len() != 1
        || reviewer_rules[0]
            .get("reviewers")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        bail!("{ENVIRONMENT} must have at least one required reviewer");
    }
    if secrets.get("total_count").and_then(Value::as_u64) != Some(0) {
        bail!("{ENVIRONMENT} must contain zero secrets");
    }
    Ok(())
}

fn validate_active_jit_runner(runner: &Value) -> Result<()> {
    if runner.get("status").and_then(Value::as_str) != Some("online")
        || runner.get("busy").and_then(Value::as_bool) != Some(true)
    {
        bail!(
            "{GROUP} contains an idle/offline registered runner; persistent runners must remain \
             queue-ineligible"
        );
    }
    let name = runner
        .get("name")
        .and_then(Value::as_str)
        .context("runner response has no name")?;
    let pattern = Regex::new(r"^fmf-perf-jit-([1-9][0-9]*)-([1-9][0-9]*)$")
        .context("invalid built-in JIT runner name pattern")?;
    let captures = pattern
        .captures(name)
        .with_context(|| format!("active runner `{name}` does not use the run/attempt JIT name"))?;
    let expected_unique = format!(
        "fmf-jit-run-{}-attempt-{}",
        captures.get(1).context("JIT name has no run id")?.as_str(),
        captures
            .get(2)
            .context("JIT name has no run attempt")?
            .as_str()
    );
    let labels = runner
        .get("labels")
        .and_then(Value::as_array)
        .context("runner response has no labels")?
        .iter()
        .map(|label| {
            label
                .get("name")
                .and_then(Value::as_str)
                .context("runner label has no name")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    for required in ["Windows", "X64", "fmf-perf", EPHEMERAL_LABEL] {
        if !labels.contains(required) {
            bail!("active {GROUP} runner is missing required label `{required}`");
        }
    }
    let unique_labels: Vec<_> = labels
        .iter()
        .filter(|label| label.starts_with("fmf-jit-run-"))
        .copied()
        .collect();
    if unique_labels != [expected_unique.as_str()] {
        bail!(
            "active {GROUP} runner has the wrong one-time label: \
             expected `{expected_unique}`, got {unique_labels:?}"
        );
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let repo_view = gh_json(&[
        "repo".to_owned(),
        "view".to_owned(),
        "--json".to_owned(),
        "nameWithOwner".to_owned(),
    ])?;
    let full_name = repo_view
        .get("nameWithOwner")
        .and_then(Value::as_str)
        .context("`gh repo view` did not identify the repository")?
        .to_owned();
    let (owner, _) = full_name
        .split_once('/')
        .context("repository identity is not owner/name")?;

    let repository = gh_json(&["api".to_owned(), format!("repos/{full_name}")])?;
    let groups_response = gh_json(&[
        "api".to_owned(),
        "--paginate".to_owned(),
        "--slurp".to_owned(),
        format!(
            "orgs/{owner}/actions/runner-groups?visible_to_repository={full_name}&per_page=100"
        ),
    ])?;
    let groups = paged_items(&groups_response, "runner_groups")?;
    let group_id = groups
        .iter()
        .filter(|group| group.get("name").and_then(Value::as_str) == Some(GROUP))
        .filter_map(|group| group.get("id").and_then(Value::as_u64))
        .collect::<Vec<_>>();
    if group_id.len() != 1 {
        bail!("cannot uniquely identify `{GROUP}` before auditing its runners");
    }
    let runners_response = gh_json(&[
        "api".to_owned(),
        "--paginate".to_owned(),
        "--slurp".to_owned(),
        format!(
            "orgs/{owner}/actions/runner-groups/{}/runners?per_page=100",
            group_id[0]
        ),
    ])?;
    let environment = gh_json(&[
        "api".to_owned(),
        format!("repos/{full_name}/environments/{ENVIRONMENT}"),
    ])?;
    let secrets = gh_json(&[
        "api".to_owned(),
        format!("repos/{full_name}/environments/{ENVIRONMENT}/secrets?per_page=100"),
    ])?;

    validate(
        &repository,
        &groups_response,
        &runners_response,
        &environment,
        &secrets,
    )?;
    println!(
        "[ OK ] {GROUP}: no persistent eligible runner (zero idle registrations); JIT runners \
         require Windows/X64/fmf-perf/{EPHEMERAL_LABEL} plus exact run/attempt identity; \
         selected workflow {full_name}/{CONTROLLER}; {ENVIRONMENT} reviewer gate; zero secrets"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixtures() -> (Value, Value, Value, Value, Value) {
        (
            json!({
                "full_name": "example/find-my-files",
                "private": false,
                "owner": {"type": "Organization"}
            }),
            json!([{
                "runner_groups": [{
                    "id": 7,
                    "name": GROUP,
                    "visibility": "selected",
                    "allows_public_repositories": true,
                    "restricted_to_workflows": true,
                    "selected_workflows": [
                        "example/find-my-files/.github/workflows/performance-controller.yml@refs/heads/main"
                    ]
                }]
            }]),
            json!([{"runners": []}]),
            json!({
                "name": ENVIRONMENT,
                "can_admins_bypass": false,
                "deployment_branch_policy": {
                    "protected_branches": true,
                    "custom_branch_policies": false
                },
                "protection_rules": [{
                    "type": "required_reviewers",
                    "reviewers": [{"type": "User", "reviewer": {"login": "reviewer"}}]
                }]
            }),
            json!({"total_count": 0, "secrets": []}),
        )
    }

    #[test]
    fn accepts_the_exact_restricted_boundary() {
        let (repository, groups, runners, environment, secrets) = fixtures();
        validate(&repository, &groups, &runners, &environment, &secrets).unwrap();
    }

    #[test]
    fn rejects_workflow_or_environment_expansion() {
        let (repository, mut groups, runners, mut environment, secrets) = fixtures();
        groups[0]["runner_groups"][0]["selected_workflows"] = json!([
            "example/find-my-files/.github/workflows/performance-controller.yml@refs/heads/main",
            "example/find-my-files/.github/workflows/other.yml@refs/heads/main"
        ]);
        assert!(validate(&repository, &groups, &runners, &environment, &secrets).is_err());

        let (_, groups, runners, _, secrets) = fixtures();
        environment["can_admins_bypass"] = json!(true);
        assert!(validate(&repository, &groups, &runners, &environment, &secrets).is_err());
    }

    #[test]
    fn rejects_user_ownership_extra_runners_and_secrets() {
        let (mut repository, groups, mut runners, environment, mut secrets) = fixtures();
        repository["owner"]["type"] = json!("User");
        assert!(validate(&repository, &groups, &runners, &environment, &secrets).is_err());

        let (repository, groups, _, environment, secrets_fixture) = fixtures();
        runners[0]["runners"] = json!([{
            "name": "persistent-perf",
            "status": "online",
            "busy": false,
            "labels": [
                {"name": "Windows"},
                {"name": "X64"},
                {"name": "fmf-perf"},
                {"name": EPHEMERAL_LABEL}
            ]
        }]);
        assert!(validate(
            &repository,
            &groups,
            &runners,
            &environment,
            &secrets_fixture
        )
        .is_err());

        let (repository, groups, runners, environment, _) = fixtures();
        secrets["total_count"] = json!(1);
        assert!(validate(&repository, &groups, &runners, &environment, &secrets).is_err());
    }

    #[test]
    fn accepts_only_a_busy_run_attempt_bound_jit_registration() {
        let (repository, groups, mut runners, environment, secrets) = fixtures();
        runners[0]["runners"] = json!([{
            "name": "fmf-perf-jit-42-3",
            "status": "online",
            "busy": true,
            "labels": [
                {"name": "self-hosted"},
                {"name": "Windows"},
                {"name": "X64"},
                {"name": "fmf-perf"},
                {"name": EPHEMERAL_LABEL},
                {"name": "fmf-jit-run-42-attempt-3"}
            ]
        }]);
        validate(&repository, &groups, &runners, &environment, &secrets).unwrap();

        runners[0]["runners"][0]["name"] = json!("fmf-perf-jit-42-4");
        assert!(
            validate(&repository, &groups, &runners, &environment, &secrets).is_err(),
            "runner name and one-time label must identify the same attempt"
        );
    }
}
