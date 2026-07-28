//! `just doctor` — a fast check that the dev environment matches `mise.toml`
//! and the gate prerequisites, so a contributor knows right after `just setup`
//! whether anything is off.
//!
//! The pure helpers (pin parsing, CI-mirror parity, version matching, rendering)
//! are unit-tested; `run` is the only part that shells out to the tools.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{bail, Result};
use toml_edit::DocumentMut;
use yaml_rust2::parser::{Event, EventReceiver, Parser};
use yaml_rust2::yaml::Hash;
use yaml_rust2::{Yaml, YamlLoader};

use crate::{cmd, paths};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Status {
    const fn tag(self) -> &'static str {
        match self {
            Self::Ok => "[ OK ]",
            Self::Info => "[INFO]",
            Self::Warn => "[WARN]",
            Self::Fail => "[FAIL]",
        }
    }
}

struct Check {
    name: String,
    status: Status,
    detail: String,
}

impl Check {
    fn new(status: Status, name: &str, detail: &str) -> Self {
        Self {
            name: name.to_owned(),
            status,
            detail: detail.to_owned(),
        }
    }
    fn ok(name: &str, detail: &str) -> Self {
        Self::new(Status::Ok, name, detail)
    }
    fn info(name: &str, detail: &str) -> Self {
        Self::new(Status::Info, name, detail)
    }
    fn fail(name: &str, detail: &str) -> Self {
        Self::new(Status::Fail, name, detail)
    }
}

// Pull every tool pin out of mise.toml. Most pins are bare strings; download
// backends use a table with a `version` field so identity, URL, and checksum can
// stay together. Backend-qualified tools are kept: a required subcommand must
// not disappear behind a falsely green doctor.
fn parse_mise_pins(mise_toml: &str) -> BTreeMap<String, String> {
    let mut pins = BTreeMap::new();
    let Ok(doc) = mise_toml.parse::<DocumentMut>() else {
        return pins;
    };
    let Some(tools) = doc.get("tools").and_then(|t| t.as_table()) else {
        return pins;
    };
    for (key, value) in tools {
        let version = value
            .as_str()
            .or_else(|| value.get("version").and_then(|v| v.as_str()));
        if let Some(v) = version {
            pins.insert(key.to_owned(), v.to_owned());
        }
    }
    pins
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WinAppArtifactPin {
    version: String,
    sha256: String,
    size: String,
}

fn parse_mise_winapp_pin(mise_toml: &str) -> std::result::Result<WinAppArtifactPin, String> {
    let doc = mise_toml
        .parse::<DocumentMut>()
        .map_err(|error| format!("invalid mise.toml: {error}"))?;
    let table = doc
        .get("tools")
        .and_then(|item| item.as_table())
        .and_then(|tools| tools.get("http:winappcli"))
        .and_then(|item| item.as_table())
        .ok_or_else(|| "missing `[tools.\"http:winappcli\"]` table".to_owned())?;
    let version = table
        .get("version")
        .and_then(|item| item.as_str())
        .filter(|version| !version.is_empty())
        .ok_or_else(|| "WinAppCli `version` must be a nonempty string".to_owned())?
        .to_owned();
    let checksum = table
        .get("checksum")
        .and_then(|item| item.as_str())
        .and_then(|checksum| checksum.strip_prefix("sha256:"))
        .ok_or_else(|| "WinAppCli `checksum` must use `sha256:<hex>`".to_owned())?
        .to_ascii_lowercase();
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("WinAppCli SHA-256 must contain exactly 64 hex digits".to_owned());
    }
    let size = table
        .get("size")
        .and_then(|item| {
            item.as_str()
                .map(str::to_owned)
                .or_else(|| item.as_integer().map(|value| value.to_string()))
        })
        .ok_or_else(|| "WinAppCli `size` must be a string or integer".to_owned())?;
    if size.parse::<u64>().ok().is_none_or(|parsed| parsed == 0) {
        return Err("WinAppCli `size` must be a positive integer".to_owned());
    }

    Ok(WinAppArtifactPin {
        version,
        sha256: checksum,
        size,
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CiStep {
    uses: Option<String>,
    id: Option<String>,
    run: Option<String>,
    inputs: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
}

#[derive(Debug)]
struct CiYaml {
    root: Yaml,
    step_blocks: Vec<Vec<CiStep>>,
}

#[derive(Default)]
struct YamlFeatureGuard {
    unsupported_reference_or_tag: bool,
}

impl EventReceiver for YamlFeatureGuard {
    fn on_event(&mut self, event: Event) {
        self.unsupported_reference_or_tag |= match event {
            Event::Alias(_) => true,
            Event::Scalar(_, _, anchor, tag)
            | Event::SequenceStart(anchor, tag)
            | Event::MappingStart(anchor, tag) => anchor != 0 || tag.is_some(),
            _ => false,
        };
    }
}

fn yaml_field<'a>(mapping: &'a Hash, key: &str) -> Option<&'a Yaml> {
    mapping.get(&Yaml::String(key.to_owned()))
}

fn yaml_mapping<'a>(node: &'a Yaml, context: &str) -> std::result::Result<&'a Hash, String> {
    node.as_hash()
        .ok_or_else(|| format!("{context} must be a YAML mapping"))
}

fn validate_yaml_node(node: &Yaml, context: &str) -> std::result::Result<(), String> {
    match node {
        Yaml::Hash(mapping) => {
            for (key, value) in mapping {
                if key.as_str().is_none() {
                    return Err(format!("{context} contains a non-string mapping key"));
                }
                validate_yaml_node(value, context)?;
            }
        }
        Yaml::Array(values) => {
            for value in values {
                validate_yaml_node(value, context)?;
            }
        }
        Yaml::Alias(_) | Yaml::BadValue => {
            return Err(format!("{context} contains an unsupported YAML node"));
        }
        Yaml::Real(_) | Yaml::Integer(_) | Yaml::String(_) | Yaml::Boolean(_) | Yaml::Null => {}
    }
    Ok(())
}

fn load_ci_yaml(path: &str, source: &str) -> std::result::Result<CiYaml, String> {
    let mut guard = YamlFeatureGuard::default();
    let mut parser = Parser::new_from_str(source);
    parser
        .load(&mut guard, true)
        .map_err(|error| format!("{path}: invalid YAML: {error}"))?;
    if guard.unsupported_reference_or_tag {
        return Err(format!(
            "{path}: YAML aliases, anchors, and explicit tags are forbidden"
        ));
    }

    // YamlLoader is itself a parser-event receiver. In addition to producing the
    // typed tree, it rejects duplicate keys at every mapping depth before a
    // LinkedHashMap could hide the earlier value.
    let documents = YamlLoader::load_from_str(source)
        .map_err(|error| format!("{path}: invalid YAML: {error}"))?;
    if documents.len() != 1 {
        return Err(format!(
            "{path}: expected exactly one YAML document, found {}",
            documents.len()
        ));
    }
    let Some(root) = documents.into_iter().next() else {
        return Err(format!("{path}: YAML document is empty"));
    };
    validate_yaml_node(&root, path)?;
    let root_mapping = yaml_mapping(&root, path)?;

    let step_blocks = if path.starts_with(".github/workflows/") {
        let jobs = yaml_field(root_mapping, "jobs")
            .ok_or_else(|| format!("{path}: workflow has no jobs mapping"))?;
        let jobs = yaml_mapping(jobs, &format!("{path}: jobs"))?;
        let mut blocks = Vec::new();
        for (job_name, job) in jobs {
            let job_name = job_name
                .as_str()
                .ok_or_else(|| format!("{path}: job name must be a string"))?;
            let job = yaml_mapping(job, &format!("{path}: job {job_name}"))?;
            if let Some(steps) = yaml_field(job, "steps") {
                blocks.push(parse_ci_steps(
                    steps,
                    &format!("{path}: job {job_name}.steps"),
                )?);
            } else if yaml_field(job, "uses").and_then(Yaml::as_str).is_none() {
                return Err(format!(
                    "{path}: job {job_name} must define either steps or a reusable workflow"
                ));
            }
        }
        blocks
    } else if path.starts_with(".github/actions/") {
        let runs = yaml_field(root_mapping, "runs")
            .ok_or_else(|| format!("{path}: action has no runs mapping"))?;
        let runs = yaml_mapping(runs, &format!("{path}: runs"))?;
        if yaml_field(runs, "using").and_then(Yaml::as_str) != Some("composite") {
            return Err(format!("{path}: repository-owned action must be composite"));
        }
        let steps = yaml_field(runs, "steps")
            .ok_or_else(|| format!("{path}: composite action has no steps"))?;
        vec![parse_ci_steps(steps, &format!("{path}: runs.steps"))?]
    } else {
        return Err(format!("{path}: unsupported CI YAML location"));
    };

    Ok(CiYaml { root, step_blocks })
}

fn optional_step_string(
    step: &Hash,
    key: &str,
    context: &str,
) -> std::result::Result<Option<String>, String> {
    let Some(value) = yaml_field(step, key) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{context}.{key} must be a string"))
        .map(Some)
}

fn selected_step_mapping(
    step: &Hash,
    field: &str,
    selected: &[&str],
    context: &str,
) -> std::result::Result<BTreeMap<String, String>, String> {
    let Some(values) = yaml_field(step, field) else {
        return Ok(BTreeMap::new());
    };
    let values = yaml_mapping(values, &format!("{context}.{field}"))?;
    let mut selected_values = BTreeMap::new();
    for key in selected {
        let Some(value) = yaml_field(values, key) else {
            continue;
        };
        let value = value
            .as_str()
            .ok_or_else(|| format!("{context}.{field}.{key} must be a string"))?;
        selected_values.insert((*key).to_owned(), value.to_owned());
    }
    Ok(selected_values)
}

fn parse_ci_steps(node: &Yaml, context: &str) -> std::result::Result<Vec<CiStep>, String> {
    let steps = node
        .as_vec()
        .ok_or_else(|| format!("{context} must be a YAML sequence"))?;
    let mut parsed = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter().enumerate() {
        let context = format!("{context}[{index}]");
        let step = yaml_mapping(step, &context)?;
        let uses = optional_step_string(step, "uses", &context)?;
        let run = optional_step_string(step, "run", &context)?;
        if uses.is_some() == run.is_some() {
            return Err(format!(
                "{context} must define exactly one string `uses` or `run`"
            ));
        }
        parsed.push(CiStep {
            uses,
            id: optional_step_string(step, "id", &context)?,
            run,
            inputs: selected_step_mapping(
                step,
                "with",
                &["dotnet-version", "tool", "fallback"],
                &context,
            )?,
            env: selected_step_mapping(
                step,
                "env",
                &[
                    "FMF_UI_CLI",
                    "ACTIONLINT_VERSION",
                    "ACTIONLINT_SHA256",
                    "ACTIONLINT_SIZE",
                    "WINAPP_VERSION",
                    "WINAPP_SHA256",
                    "WINAPP_SIZE",
                ],
                &context,
            )?,
        });
    }
    Ok(parsed)
}

// Explicit aliases for CI binaries whose upstream executable/package name is
// not the mise key. Adding a new mirrored taiki-e tool without updating this
// policy is a hard failure instead of a silently unverified duplicate pin.
fn taiki_mise_key(tool: &str) -> Option<&'static str> {
    match tool {
        "just" => Some("just"),
        "mdbook" => Some("cargo:mdbook"),
        "cargo-about" => Some("cargo:cargo-about"),
        "cargo-nextest" | "nextest" => Some("cargo:cargo-nextest"),
        "cargo-deny" => Some("cargo:cargo-deny"),
        "cargo-machete" => Some("cargo:cargo-machete"),
        "cargo-llvm-cov" => Some("cargo:cargo-llvm-cov"),
        "cargo-mutants" => Some("cargo:cargo-mutants"),
        "taplo" | "taplo-cli" => Some("cargo:taplo-cli"),
        "typos" | "typos-cli" => Some("cargo:typos-cli"),
        "zizmor" => Some("github:zizmorcore/zizmor"),
        _ => None,
    }
}

// These binaries intentionally have no local mise mirror. They are narrow,
// CI-only tools: advisory scanning and remote SBOM monitoring. Every exception
// remains version-pinned in YAML. cargo-fuzz is audited separately because its
// upstream has no checksummed prebuilt manifest.
fn taiki_mirror_exception(tool: &str) -> Option<&'static str> {
    match tool {
        "cargo-audit" => Some("CI-only advisory scanner"),
        "osv-scanner" => Some("CI-only release SBOM monitor"),
        _ => None,
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct MirrorParity {
    compared: usize,
    excepted: usize,
    problems: Vec<String>,
}

fn ci_env_values<'a>(ci: &'a CiYaml, key: &str) -> Vec<&'a str> {
    ci.step_blocks
        .iter()
        .flatten()
        .filter_map(|step| step.env.get(key).map(String::as_str))
        .collect()
}

fn ci_scripts(ci: &CiYaml) -> Vec<&str> {
    ci.step_blocks
        .iter()
        .flatten()
        .filter_map(|step| step.run.as_deref())
        .collect()
}

fn action_output_value<'a>(ci: &'a CiYaml, output: &str) -> Option<&'a str> {
    let root = ci.root.as_hash()?;
    let outputs = yaml_field(root, "outputs")?.as_hash()?;
    let output = yaml_field(outputs, output)?.as_hash()?;
    yaml_field(output, "value")?.as_str()
}

fn compare_winapp_action_pin(parity: &mut MirrorParity, pin: &WinAppArtifactPin, ci: &CiYaml) {
    for (constant, expected) in [
        ("WINAPP_VERSION", pin.version.as_str()),
        ("WINAPP_SHA256", pin.sha256.as_str()),
        ("WINAPP_SIZE", pin.size.as_str()),
    ] {
        let values = ci_env_values(ci, constant);
        if values.len() != 1 {
            parity.problems.push(format!(
                ".github/actions/setup-winapp/action.yml: expected exactly one fixed `{constant}`, found {}",
                values.len()
            ));
            continue;
        }
        parity.compared += 1;
        let actual = if constant == "WINAPP_SHA256" {
            values[0].to_ascii_lowercase()
        } else {
            values[0].to_owned()
        };
        if actual != expected {
            parity.problems.push(format!(
                ".github/actions/setup-winapp/action.yml: `{constant}` is {actual}, mise.toml pins {expected}"
            ));
        }
    }

    if action_output_value(ci, "cli-path") != Some("${{ steps.install.outputs.cli-path }}") {
        parity.problems.push(
            ".github/actions/setup-winapp/action.yml: cli-path output must map exactly to steps.install.outputs.cli-path"
                .to_owned(),
        );
    }
    let install_steps = ci
        .step_blocks
        .iter()
        .flatten()
        .filter(|step| step.id.as_deref() == Some("install"))
        .count();
    if install_steps != 1 {
        parity.problems.push(format!(
            ".github/actions/setup-winapp/action.yml: expected exactly one `install` step, found {install_steps}"
        ));
    }
    let scripts = ci_scripts(ci);
    for required in ["$env:RUNNER_TEMP", "$env:GITHUB_OUTPUT"] {
        if !scripts.iter().any(|script| script.contains(required)) {
            parity.problems.push(format!(
                ".github/actions/setup-winapp/action.yml: missing required runner-temp/output boundary `{required}`"
            ));
        }
    }
    for forbidden in ["$env:GITHUB_WORKSPACE", "build/tools/winapp"] {
        if scripts.iter().any(|script| script.contains(forbidden)) {
            parity.problems.push(format!(
                ".github/actions/setup-winapp/action.yml: verified tool must not use checkout path `{forbidden}`"
            ));
        }
    }
}

fn compare_actionlint_action_pin(
    parity: &mut MirrorParity,
    pins: &BTreeMap<String, String>,
    ci: &CiYaml,
) {
    let versions = ci_env_values(ci, "ACTIONLINT_VERSION");
    if versions.len() == 1 {
        compare_mirror_pin(
            parity,
            pins,
            ".github/actions/setup-actionlint/action.yml",
            "github:rhysd/actionlint",
            versions[0],
        );
    } else {
        parity.problems.push(format!(
            ".github/actions/setup-actionlint/action.yml: expected exactly one fixed `ACTIONLINT_VERSION`, found {}",
            versions.len()
        ));
    }

    let digests = ci_env_values(ci, "ACTIONLINT_SHA256");
    if digests.len() != 1
        || digests[0].len() != 64
        || !digests[0].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        parity.problems.push(
            ".github/actions/setup-actionlint/action.yml: ACTIONLINT_SHA256 must be one fixed 64-digit hex digest"
                .to_owned(),
        );
    }
    let sizes = ci_env_values(ci, "ACTIONLINT_SIZE");
    if sizes.len() != 1
        || sizes[0]
            .parse::<u64>()
            .ok()
            .is_none_or(|parsed| parsed == 0)
    {
        parity.problems.push(
            ".github/actions/setup-actionlint/action.yml: ACTIONLINT_SIZE must be one fixed positive integer"
                .to_owned(),
        );
    }
    let scripts = ci_scripts(ci);
    for required in [
        "${RUNNER_TEMP}",
        "stat --format='%s'",
        "sha256sum --check --strict",
        "tar --extract",
        "$GITHUB_OUTPUT",
        "executable=",
    ] {
        if !scripts.iter().any(|script| script.contains(required)) {
            parity.problems.push(format!(
                ".github/actions/setup-actionlint/action.yml: missing fail-closed verifier `{required}`"
            ));
        }
    }
    if scripts
        .iter()
        .any(|script| script.contains("GITHUB_WORKSPACE"))
    {
        parity.problems.push(
            ".github/actions/setup-actionlint/action.yml: verified tool must not be installed into the checkout"
                .to_owned(),
        );
    }
    if scripts.iter().any(|script| script.contains("GITHUB_PATH")) {
        parity.problems.push(
            ".github/actions/setup-actionlint/action.yml: verified executable must be consumed by absolute output, not PATH mutation"
                .to_owned(),
        );
    }
}

fn compare_mirror_pin(
    parity: &mut MirrorParity,
    pins: &BTreeMap<String, String>,
    location: &str,
    pin_key: &str,
    workflow_version: &str,
) {
    parity.compared += 1;
    let Some(mise_version) = pins.get(pin_key) else {
        parity.problems.push(format!(
            "{location}: `{pin_key}` has a CI mirror but no mise.toml pin"
        ));
        return;
    };
    if workflow_version != mise_version {
        parity.problems.push(format!(
            "{location}: CI pins {workflow_version}, mise.toml `{pin_key}` pins {mise_version}"
        ));
    }
}

fn workflow_runs_ui_automation(ci: &CiYaml) -> bool {
    ci.step_blocks.iter().flatten().any(|step| {
        step.run
            .as_deref()
            .is_some_and(|command| command.contains("just ui-test"))
    })
}

fn ui_cli_wiring_problems(path: &str, ci: &CiYaml) -> Vec<String> {
    let mut problems = Vec::new();
    for block in &ci.step_blocks {
        let mut setup_ids = Vec::new();
        for step in block {
            if step.uses.as_deref() == Some("./.github/actions/setup-winapp") {
                match step.id.as_deref().filter(|id| !id.is_empty()) {
                    Some(id) => setup_ids.push(id.to_owned()),
                    None => problems.push(format!(
                        "{path}: setup-winapp must have an id so consumers can use its verified output"
                    )),
                }
            }
            if !step
                .run
                .as_deref()
                .is_some_and(|command| command.contains("just ui-test"))
            {
                continue;
            }
            let actual = step.env.get("FMF_UI_CLI").map(String::as_str);
            let valid = setup_ids.iter().any(|id| {
                let expected = format!("${{{{ steps.{id}.outputs.cli-path }}}}");
                actual == Some(expected.as_str())
            });
            if !valid {
                problems.push(format!(
                    "{path}: UI automation must consume a preceding same-job setup-winapp `cli-path` through FMF_UI_CLI"
                ));
            }
        }
    }
    problems
}

fn ci_mirror_parity(
    workflows: &[(&str, &str)],
    pins: &BTreeMap<String, String>,
    winapp_pin: &WinAppArtifactPin,
) -> MirrorParity {
    let mut parity = MirrorParity::default();
    let mut saw_dotnet = false;
    let mut saw_winapp = false;
    let mut saw_actionlint = false;
    let mut saw_taiki = false;
    let mut cargo_fuzz_installs = 0;
    let mut cargo_sbom_installs = 0;
    let mut validated_winapp_action = false;
    let mut validated_actionlint_action = false;

    for (path, source) in workflows {
        let ci = match load_ci_yaml(path, source) {
            Ok(ci) => ci,
            Err(problem) => {
                parity.problems.push(problem);
                continue;
            }
        };
        if *path == ".github/actions/setup-winapp/action.yml" {
            compare_winapp_action_pin(&mut parity, winapp_pin, &ci);
            validated_winapp_action = true;
        } else if *path == ".github/actions/setup-actionlint/action.yml" {
            compare_actionlint_action_pin(&mut parity, pins, &ci);
            validated_actionlint_action = true;
        }

        let scripts = ci_scripts(&ci);
        for script in &scripts {
            for line in script.lines() {
                let command = line.trim();
                if command.starts_with('#') {
                    continue;
                }
                if command.contains("cargo install cargo-fuzz") {
                    cargo_fuzz_installs += 1;
                    let expected = "cargo install cargo-fuzz --locked --version 0.13.2 --root \"$RUNNER_TEMP/fmf-cargo-fuzz\"";
                    if *path != ".github/workflows/fuzz.yml" || command != expected {
                        parity.problems.push(format!(
                        "{path}: cargo-fuzz must be the sole audited locked crates.io install at the fixed runner-temp root"
                    ));
                    } else {
                        parity.excepted += 1;
                    }
                }
                if command.contains("cargo install cargo-sbom") {
                    cargo_sbom_installs += 1;
                    let expected =
                        "cargo install cargo-sbom --locked --version 0.10.0 --root $cargoRoot";
                    let fixed_root = scripts.iter().any(|script| {
                        script
                            .contains("$cargoRoot = Join-Path $env:RUNNER_TEMP \"fmf-cargo-sbom\"")
                    });
                    if *path != ".github/actions/sbom-scan/action.yml"
                        || command != expected
                        || !fixed_root
                    {
                        parity.problems.push(format!(
                        "{path}: cargo-sbom must be the sole audited locked crates.io install under runner temp"
                    ));
                    } else {
                        parity.excepted += 1;
                    }
                }
            }
        }
        let requires_winapp = workflow_runs_ui_automation(&ci);
        let mut has_winapp = false;
        for step in ci.step_blocks.iter().flatten() {
            let Some(action_ref) = step.uses.as_deref() else {
                continue;
            };
            let action_name = action_ref
                .rsplit_once('@')
                .map_or(action_ref, |(name, _)| name)
                .to_ascii_lowercase();
            match action_name.as_str() {
                "actions/setup-dotnet" => {
                    saw_dotnet = true;
                    if let Some(version) = step.inputs.get("dotnet-version") {
                        compare_mirror_pin(&mut parity, pins, path, "dotnet", version);
                    } else {
                        parity
                            .problems
                            .push(format!("{path}: setup-dotnet has no `dotnet-version`"));
                    }
                }
                "./.github/actions/setup-winapp" => {
                    saw_winapp = true;
                    has_winapp = true;
                }
                "./.github/actions/setup-actionlint" => {
                    saw_actionlint = true;
                }
                "microsoft/setup-winappcli" => {
                    parity.problems.push(format!(
                        "{path}: external setup-WinAppCli bypasses the repository's fixed local verifier"
                    ));
                }
                "raven-actions/actionlint" | "crate-ci/typos" | "uncenter/setup-taplo" => {
                    parity.problems.push(format!(
                        "{path}: forbidden opaque setup/lint Action `{action_name}`; use the repository-owned verifier or checksummed binary installer"
                    ));
                }
                "taiki-e/install-action" => {
                    saw_taiki = true;
                    if step.inputs.get("fallback").map(String::as_str) != Some("none") {
                        parity.problems.push(format!(
                            "{path}: install-action must set `fallback: none` to forbid \
                             quickinstall/source-install fallback"
                        ));
                    }
                    let Some(tool_list) = step.inputs.get("tool") else {
                        parity
                            .problems
                            .push(format!("{path}: install-action has no `tool` input"));
                        continue;
                    };
                    let mut found_tool = false;
                    for token in tool_list
                        .split(|character: char| character == ',' || character.is_whitespace())
                    {
                        if token.is_empty() {
                            continue;
                        }
                        found_tool = true;
                        let Some((tool, version)) = token.rsplit_once('@') else {
                            parity.problems.push(format!(
                                "{path}: taiki-e tool `{token}` is not version-pinned"
                            ));
                            continue;
                        };
                        if let Some(pin_key) = taiki_mise_key(tool) {
                            compare_mirror_pin(&mut parity, pins, path, pin_key, version);
                        } else if taiki_mirror_exception(tool).is_some() {
                            parity.excepted += 1;
                        } else {
                            parity.problems.push(format!(
                                "{path}: taiki-e tool `{tool}` has no mise mirror or explicit exception"
                            ));
                        }
                    }
                    if !found_tool {
                        parity
                            .problems
                            .push(format!("{path}: install-action has an empty `tool` input"));
                    }
                }
                _ => {}
            }
        }
        parity.problems.extend(ui_cli_wiring_problems(path, &ci));
        if requires_winapp && !has_winapp {
            parity.problems.push(format!(
                "{path}: runs UI automation without ./.github/actions/setup-winapp"
            ));
        }
    }

    if !saw_dotnet {
        parity
            .problems
            .push("no actions/setup-dotnet mirror found in workflows".to_owned());
    }
    if !saw_winapp {
        parity
            .problems
            .push("no ./.github/actions/setup-winapp wiring found in workflows".to_owned());
    }
    if !saw_actionlint {
        parity
            .problems
            .push("no ./.github/actions/setup-actionlint wiring found in workflows".to_owned());
    }
    if !saw_taiki {
        parity
            .problems
            .push("no taiki-e/install-action mirror found in workflows".to_owned());
    }
    if !validated_winapp_action {
        parity
            .problems
            .push("setup-winapp action was not parsed and validated".to_owned());
    }
    if !validated_actionlint_action {
        parity
            .problems
            .push("setup-actionlint action was not parsed and validated".to_owned());
    }
    if cargo_fuzz_installs != 1 {
        parity.problems.push(format!(
            "expected exactly one audited cargo-fuzz source install, found {cargo_fuzz_installs}"
        ));
    }
    if cargo_sbom_installs != 1 {
        parity.problems.push(format!(
            "expected exactly one audited cargo-sbom source install, found {cargo_sbom_installs}"
        ));
    }
    parity
}

fn ci_mirror_check_at(root: &Path, pins: &BTreeMap<String, String>, mise_toml: &str) -> Check {
    let winapp_pin = match parse_mise_winapp_pin(mise_toml) {
        Ok(pin) => pin,
        Err(error) => {
            return Check::fail(
                "CI mirror parity",
                &format!("invalid local WinAppCli pin: {error}"),
            );
        }
    };
    let workflow_dir = root.join(".github").join("workflows");
    let Ok(entries) = std::fs::read_dir(&workflow_dir) else {
        return Check::fail(
            "CI mirror parity",
            "cannot read .github/workflows — CI pins are unverified",
        );
    };
    let mut paths: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "yml" | "yaml"))
        })
        .collect();
    let action_dir = root.join(".github").join("actions");
    if let Ok(actions) = std::fs::read_dir(action_dir) {
        for action in actions.filter_map(Result::ok) {
            if !action.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            for filename in ["action.yml", "action.yaml"] {
                let path = action.path().join(filename);
                if path.is_file() {
                    paths.push(path);
                }
            }
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Check::fail(
            "CI mirror parity",
            "no workflow YAML found — CI pins are unverified",
        );
    }

    let mut owned_sources = Vec::with_capacity(paths.len());
    for path in paths {
        let display = path
            .strip_prefix(root)
            .map_or_else(
                |_| path.display().to_string(),
                |rel| rel.display().to_string(),
            )
            .replace('\\', "/");
        let Ok(source) = std::fs::read_to_string(&path) else {
            return Check::fail(
                "CI mirror parity",
                &format!("cannot read {display} — CI pins are unverified"),
            );
        };
        owned_sources.push((display, source));
    }
    let sources: Vec<_> = owned_sources
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let parity = ci_mirror_parity(&sources, pins, &winapp_pin);

    if parity.problems.is_empty() {
        Check::ok(
            "CI mirror parity",
            &format!(
                "{} mirrored pin(s) match mise.toml; {} explicit CI-only exception(s)",
                parity.compared, parity.excepted
            ),
        )
    } else {
        Check::fail(
            "CI mirror parity",
            &format!(
                "{} problem(s): {}",
                parity.problems.len(),
                parity.problems.join(" | ")
            ),
        )
    }
}

// Whether `actual` satisfies the loose mise `pin`: each dot-separated part of the
// pin must equal the matching leading part of `actual`. Pin `10` accepts
// `10.0.118`; pin `1.95` accepts `1.95.3` but not `1.96.0`.
fn version_satisfies(pin: &str, actual: &str) -> bool {
    let mut actual_parts = actual.split('.');
    for p in pin.split('.') {
        if actual_parts.next() != Some(p) {
            return false;
        }
    }
    true
}

// Return the first whitespace-separated version token, accepting an optional
// leading `v` as well as output such as `rustc 1.95.0`.
fn first_version_token(raw: &str) -> Option<&str> {
    raw.split_whitespace().find_map(|tok| {
        let numeric = tok.strip_prefix('v').unwrap_or(tok);
        numeric
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_digit)
            .then_some(numeric)
    })
}

// Probe one tool: compare the version it reports against its mise pin.
fn tool_check(
    name: &str,
    pin_key: &str,
    program: &str,
    args: &[&str],
    pins: &BTreeMap<String, String>,
) -> Check {
    let Some(pin) = pins.get(pin_key) else {
        return Check::fail(name, "not pinned in mise.toml");
    };
    let Some(raw) = cmd::capture(&paths::repo_root(), program, args) else {
        return Check::fail(
            name,
            &format!(
                "pinned {pin}, but the version probe via `{program}` failed — run `mise install`"
            ),
        );
    };
    let Some(actual) = first_version_token(&raw) else {
        return Check::fail(
            name,
            &format!("pinned {pin}, but `{program}` reported an unparsable version"),
        );
    };
    if version_satisfies(pin, actual) {
        Check::ok(name, &format!("{actual} (pin {pin})"))
    } else {
        Check::fail(
            name,
            &format!("pinned {pin}, found {actual} — run `mise install`"),
        )
    }
}

#[cfg(windows)]
fn elevation_detail() -> String {
    // High Mandatory Level (S-1-16-12288) or System (S-1-16-16384) in whoami's
    // group list means an elevated token — no extra crate dependency needed.
    match cmd::capture(&paths::repo_root(), "whoami", &["/groups"]) {
        Some(groups) if groups.contains("S-1-16-12288") || groups.contains("S-1-16-16384") => {
            "ADMIN — full $MFT / USN access".to_owned()
        }
        Some(_) => "standard — index / bench / service recipes need an elevated shell".to_owned(),
        None => "unknown (whoami unavailable)".to_owned(),
    }
}

#[cfg(not(windows))]
fn elevation_detail() -> String {
    "n/a (non-Windows host)".to_owned()
}

// ADR-0021: every generated artifact except C# obj lives under build/. The
// required `build/` ignore rule also matches nested directories, and the WinUI
// project's local ignore file masks its conventional output directories, so
// doctor probes these paths directly instead of relying on `git status`.
const MISPLACED_OUTPUT_PATHS: &[&str] = &[
    "target",
    "engine/target",
    "xtask/target",
    "engine/fuzz/target",
    "engine/engine-lcov.info",
    "engine/mutants.out",
    "engine/mutants.out.old",
    "docfx/build",
    "app/FindMyFiles.Tests/build",
    "app/FindMyFiles.Tests/coverage.json",
    "app/FindMyFiles.Tests/TestResults",
    "app/FindMyFiles.Tests/StrykerOutput",
    "app/FindMyFiles.Tests/UiAutomation/artifacts",
];

const APP_LOCAL_OUTPUT_DIR_NAMES: &[&str] = &[
    "bin",
    "debug",
    "release",
    "artifacts",
    "log",
    "logs",
    "AppPackages",
    "BundleArtifacts",
    "publish",
    "PublishScripts",
    "Generated Files",
];

fn is_app_local_output_dir(name: &str) -> bool {
    APP_LOCAL_OUTPUT_DIR_NAMES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
        || name
            .get(.."testresult".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("testresult"))
}

fn build_layout_strays(root: &Path) -> Vec<String> {
    let mut strays: Vec<String> = MISPLACED_OUTPUT_PATHS
        .iter()
        .filter(|rel| root.join(rel).exists())
        .map(|rel| (*rel).to_owned())
        .collect();

    let app_dir = root.join("app").join("FindMyFiles");
    if let Ok(entries) = std::fs::read_dir(app_dir) {
        for entry in entries.filter_map(Result::ok) {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if is_app_local_output_dir(&name) {
                strays.push(format!("app/FindMyFiles/{name}"));
            }
        }
    }

    strays.sort();
    strays.dedup();
    strays
}

fn build_layout_check_at(root: &Path) -> Check {
    let strays = build_layout_strays(root);
    if strays.is_empty() {
        Check::ok(
            "build/ layout",
            "no generated output outside build/ (ADR-0021)",
        )
    } else {
        let paths = strays.join(", ");
        Check::fail(
            "build/ layout",
            &format!(
                "stray generated path(s): {paths} — delete; output belongs under build/ (ADR-0021)"
            ),
        )
    }
}

fn build_layout_check() -> Check {
    let root = paths::repo_root();
    build_layout_check_at(&root)
}

fn render(checks: &[Check]) -> String {
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut out = String::from("\nfind-my-files doctor\n\n");
    for c in checks {
        let _ = writeln!(
            out,
            "  {tag}  {name:<width$}  {detail}",
            tag = c.status.tag(),
            name = c.name,
            detail = c.detail,
        );
    }
    out
}

fn overall(checks: &[Check]) -> Status {
    if checks.iter().any(|c| c.status == Status::Fail) {
        Status::Fail
    } else if checks.iter().any(|c| c.status == Status::Warn) {
        Status::Warn
    } else {
        Status::Ok
    }
}

/// Print the environment report and fail (non-zero exit) only when a `[FAIL]`
/// item is present — a `[WARN]` leaves the environment usable.
pub fn run() -> Result<()> {
    let mise_toml = std::fs::read_to_string(paths::mise_toml()).unwrap_or_default();
    let pins = parse_mise_pins(&mise_toml);

    let mise = if matches!(
        cmd::succeeds(&paths::repo_root(), "mise", &["--version"]),
        Ok(true)
    ) {
        Check::ok("mise", "present")
    } else {
        Check::fail(
            "mise",
            "not found — install mise, then `mise install` (see README)",
        )
    };
    let mut checks = vec![mise];

    for (name, pin_key, program, args) in [
        ("rust", "rust", "rustc", &["--version"][..]),
        ("dotnet", "dotnet", "dotnet", &["--version"][..]),
        (
            "cargo-binstall",
            "cargo-binstall",
            "mise",
            &["exec", "cargo-binstall", "--command", "cargo-binstall -V"][..],
        ),
        (
            "winapp",
            "http:winappcli",
            "mise",
            &["exec", "http:winappcli", "--command", "winapp --version"][..],
        ),
        ("just", "just", "just", &["--version"][..]),
        ("lefthook", "lefthook", "lefthook", &["version"][..]),
        (
            "cargo-llvm-cov",
            "cargo:cargo-llvm-cov",
            "cargo",
            &["llvm-cov", "--version"][..],
        ),
        (
            "cargo-nextest",
            "cargo:cargo-nextest",
            "cargo",
            &["nextest", "--version"][..],
        ),
        (
            "cargo-deny",
            "cargo:cargo-deny",
            "cargo",
            &["deny", "--version"][..],
        ),
        (
            "cargo-machete",
            "cargo:cargo-machete",
            "cargo-machete",
            &["--version"][..],
        ),
        (
            "cargo-about",
            "cargo:cargo-about",
            "cargo",
            &["about", "--version"][..],
        ),
        (
            "cargo-mutants",
            "cargo:cargo-mutants",
            "cargo",
            &["mutants", "--version"][..],
        ),
        ("samply", "cargo:samply", "samply", &["--version"][..]),
        ("bacon", "cargo:bacon", "bacon", &["--version"][..]),
        (
            "mdbook",
            "cargo:mdbook",
            "mise",
            &["exec", "cargo:mdbook", "--command", "mdbook --version"][..],
        ),
        ("taplo", "cargo:taplo-cli", "taplo", &["--version"][..]),
        ("typos", "cargo:typos-cli", "typos", &["--version"][..]),
        (
            "committed",
            "cargo:committed",
            "committed",
            &["--version"][..],
        ),
        (
            "actionlint",
            "github:rhysd/actionlint",
            "actionlint",
            &["--version"][..],
        ),
        (
            "zizmor",
            "github:zizmorcore/zizmor",
            "mise",
            &[
                "exec",
                "github:zizmorcore/zizmor",
                "--command",
                "zizmor --version",
            ][..],
        ),
    ] {
        checks.push(tool_check(name, pin_key, program, args, &pins));
    }

    checks.push(ci_mirror_check_at(&paths::repo_root(), &pins, &mise_toml));
    checks.push(Check::info("elevation", &elevation_detail()));
    checks.push(build_layout_check());

    print!("{}", render(&checks));

    let fails = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warns = checks.iter().filter(|c| c.status == Status::Warn).count();
    let summary = match overall(&checks) {
        Status::Fail => format!("\n{fails} FAIL, {warns} WARN — fix the failures above\n"),
        Status::Warn => {
            format!("\n{warns} WARN — environment usable; `mise install` resolves version drift\n")
        }
        Status::Ok | Status::Info => "\nall good — environment matches mise.toml\n".to_owned(),
    };
    print!("{summary}");

    if fails > 0 {
        bail!("doctor found {fails} environment failure(s)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_backend_pins() {
        let toml = "\
[tools]
rust = \"1.95\"
dotnet = \"10\"
just = \"1\"
\"cargo:samply\" = \"0.13.1\"
\"cargo:cargo-nextest\" = \"0.9.140\"
\"github:rhysd/actionlint\" = \"1.7.7\"
\"github:zizmorcore/zizmor\" = \"1.28.0\"

[tools.\"http:winappcli\"]
version = \"0.5.0\"
url = \"https://example.invalid/winappcli-x64.zip\"
checksum = \"sha256:00\"
size = \"123\"

[settings]
cargo.binstall = true
";
        let pins = parse_mise_pins(toml);
        assert_eq!(pins.get("rust").map(String::as_str), Some("1.95"));
        assert_eq!(pins.get("dotnet").map(String::as_str), Some("10"));
        assert_eq!(
            pins.get("http:winappcli").map(String::as_str),
            Some("0.5.0")
        );
        assert_eq!(pins.get("just").map(String::as_str), Some("1"));
        assert_eq!(pins.get("cargo:samply").map(String::as_str), Some("0.13.1"));
        assert_eq!(
            pins.get("cargo:cargo-nextest").map(String::as_str),
            Some("0.9.140")
        );
        assert_eq!(
            pins.get("github:rhysd/actionlint").map(String::as_str),
            Some("1.7.7")
        );
        assert_eq!(
            pins.get("github:zizmorcore/zizmor").map(String::as_str),
            Some("1.28.0")
        );
        assert_eq!(pins.len(), 8);
    }

    #[test]
    fn parses_winapp_artifact_identity_from_mise() {
        let toml = r#"
[tools."http:winappcli"]
version = "0.5.0"
checksum = "sha256:88735CE6C2582AC5FAC6200194BF62467FDD72B44B2D230F3A4ED059FA79EE7D"
size = "37881954"
"#;

        let pin = parse_mise_winapp_pin(toml).expect("valid WinAppCli pin");

        assert_eq!(pin.version, "0.5.0");
        assert_eq!(
            pin.sha256,
            "88735ce6c2582ac5fac6200194bf62467fdd72b44b2d230f3a4ed059fa79ee7d"
        );
        assert_eq!(pin.size, "37881954");
    }

    #[test]
    fn rejects_incomplete_winapp_artifact_identity() {
        let missing_size = r#"
[tools."http:winappcli"]
version = "0.5.0"
checksum = "sha256:88735ce6c2582ac5fac6200194bf62467fdd72b44b2d230f3a4ed059fa79ee7d"
"#;
        let malformed_hash = r#"
[tools."http:winappcli"]
version = "0.5.0"
checksum = "sha256:not-a-hash"
size = 37881954
"#;

        assert!(parse_mise_winapp_pin(missing_size).is_err());
        assert!(parse_mise_winapp_pin(malformed_hash).is_err());
    }

    #[test]
    fn ignores_tool_tables_without_string_versions() {
        let toml = "\
[tools]
rust = \"1.95\"

[tools.\"http:missing-version\"]
url = \"https://example.invalid/missing.zip\"

[tools.\"http:numeric-version\"]
version = 3
url = \"https://example.invalid/numeric.zip\"
";
        let pins = parse_mise_pins(toml);
        assert_eq!(
            pins,
            BTreeMap::from([("rust".to_owned(), "1.95".to_owned())])
        );
    }

    #[test]
    fn yaml_ast_reads_workflow_steps() {
        let workflow = r#"
jobs:
  test:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/setup-dotnet@immutable
        with:
          dotnet-version: "10.0.302"
      - name: Install tools
        uses: taiki-e/install-action@immutable
        with:
          tool: |
            just@1.54.0
            cargo-nextest@0.9.140
          fallback: none
"#;
        let ci = load_ci_yaml(".github/workflows/test.yml", workflow).expect("valid workflow");
        let steps = &ci.step_blocks[0];
        assert_eq!(steps.len(), 2);
        assert_eq!(
            steps[0].uses.as_deref(),
            Some("actions/setup-dotnet@immutable")
        );
        assert_eq!(
            steps[0].inputs.get("dotnet-version").map(String::as_str),
            Some("10.0.302")
        );
        let tools: Vec<_> = steps[1]
            .inputs
            .get("tool")
            .expect("tool input")
            .split_whitespace()
            .collect();
        assert_eq!(tools, ["just@1.54.0", "cargo-nextest@0.9.140"]);
        assert_eq!(
            steps[1].inputs.get("fallback").map(String::as_str),
            Some("none")
        );
    }

    #[test]
    fn yaml_ast_rejects_aliases_multiple_documents_and_wrong_step_types() {
        let alias = "jobs:\n  first: &job\n    runs-on: ubuntu\n    steps: []\n  second: *job\n";
        let tagged = "jobs:\n  test: !custom\n    runs-on: ubuntu\n    steps: []\n";
        let multiple = "jobs: {}\n---\njobs: {}\n";
        let wrong_type = "jobs:\n  test:\n    runs-on: ubuntu\n    steps: {}\n";

        assert!(load_ci_yaml(".github/workflows/alias.yml", alias).is_err());
        assert!(load_ci_yaml(".github/workflows/tagged.yml", tagged).is_err());
        assert!(load_ci_yaml(".github/workflows/multiple.yml", multiple).is_err());
        assert!(load_ci_yaml(".github/workflows/type.yml", wrong_type).is_err());
    }

    #[test]
    fn yaml_ast_preserves_github_on_key_and_rejects_nested_duplicate_keys() {
        let workflow = "on: push\njobs:\n  test:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n";
        let duplicate = "jobs:\n  test:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ok\n        env:\n          FMF_UI_CLI: first\n          FMF_UI_CLI: second\n";

        let ci = load_ci_yaml(".github/workflows/on.yml", workflow).expect("valid GitHub workflow");
        let root = ci.root.as_hash().expect("workflow root mapping");
        assert_eq!(
            yaml_field(root, "on").and_then(Yaml::as_str),
            Some("push"),
            "yaml-rust2 must use YAML 1.2 semantics where plain `on` is a string"
        );

        let error = load_ci_yaml(".github/workflows/duplicate.yml", duplicate)
            .expect_err("nested duplicate mapping key must fail closed");
        assert!(
            error.contains("duplicated key"),
            "unexpected duplicate-key error: {error}"
        );
    }

    fn mirror_test_pins() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("dotnet".to_owned(), "10.0.302".to_owned()),
            ("http:winappcli".to_owned(), "0.5.0".to_owned()),
            ("just".to_owned(), "1.54.0".to_owned()),
            ("cargo:mdbook".to_owned(), "0.5.3".to_owned()),
            ("cargo:cargo-about".to_owned(), "0.7.1".to_owned()),
            ("cargo:cargo-nextest".to_owned(), "0.9.140".to_owned()),
            ("cargo:cargo-deny".to_owned(), "0.19.9".to_owned()),
            ("cargo:cargo-machete".to_owned(), "0.9.2".to_owned()),
            ("cargo:cargo-llvm-cov".to_owned(), "0.8.7".to_owned()),
            ("cargo:cargo-mutants".to_owned(), "27.1.0".to_owned()),
            ("cargo:taplo-cli".to_owned(), "0.10.0".to_owned()),
            ("cargo:typos-cli".to_owned(), "1.47.2".to_owned()),
            ("github:rhysd/actionlint".to_owned(), "1.7.12".to_owned()),
            ("github:zizmorcore/zizmor".to_owned(), "1.28.0".to_owned()),
        ])
    }

    fn mirror_test_winapp_pin() -> WinAppArtifactPin {
        WinAppArtifactPin {
            version: "0.5.0".to_owned(),
            sha256: "88735ce6c2582ac5fac6200194bf62467fdd72b44b2d230f3a4ed059fa79ee7d".to_owned(),
            size: "37881954".to_owned(),
        }
    }

    const MIRROR_TEST_WINAPP_ACTION: &str = r#"
outputs:
  cli-path:
    value: ${{ steps.install.outputs.cli-path }}
runs:
  using: composite
  steps:
    - shell: pwsh
      id: install
      env:
        WINAPP_VERSION: "0.5.0"
        WINAPP_SHA256: 88735ce6c2582ac5fac6200194bf62467fdd72b44b2d230f3a4ed059fa79ee7d
        WINAPP_SIZE: "37881954"
      run: |
        $install = Join-Path $env:RUNNER_TEMP "winapp"
        "cli-path=$install" | Out-File $env:GITHUB_OUTPUT
"#;

    const MIRROR_TEST_ACTIONLINT_ACTION: &str = r#"
outputs:
  executable:
    value: ${{ steps.install.outputs.executable }}
runs:
  using: composite
  steps:
    - shell: bash
      id: install
      env:
        ACTIONLINT_VERSION: "1.7.12"
        ACTIONLINT_SHA256: 8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8
        ACTIONLINT_SIZE: "2353908"
      run: |
        install_dir="${RUNNER_TEMP}/actionlint"
        stat --format='%s' archive
        sha256sum --check --strict checksums
        tar --extract archive
        printf 'executable=%s\n' "$install_dir/actionlint" >> "$GITHUB_OUTPUT"
"#;

    #[test]
    fn ci_mirror_parity_accepts_tool_aliases_and_explicit_ci_only_exceptions() {
        let workflow = r#"
jobs:
  test:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/setup-dotnet@immutable
        with:
          dotnet-version: 10.0.302
      - uses: ./.github/actions/setup-winapp
        id: winapp
      - uses: ./.github/actions/setup-actionlint
      - uses: taiki-e/install-action@immutable
        with:
          tool: |
            just@1.54.0
            mdbook@0.5.3
            cargo-about@0.7.1
            nextest@0.9.140
            cargo-deny@0.19.9
            cargo-machete@0.9.2
            cargo-llvm-cov@0.8.7
            cargo-mutants@27.1.0
            taplo@0.10.0
            typos-cli@1.47.2
            zizmor@1.28.0
            cargo-audit@0.22.2
            osv-scanner@2.3.6
          fallback: none
      - run: cargo install cargo-fuzz --locked --version 0.13.2 --root "$RUNNER_TEMP/fmf-cargo-fuzz"
"#;
        let sbom_action = r#"
runs:
  using: composite
  steps:
    - shell: pwsh
      run: |
        $cargoRoot = Join-Path $env:RUNNER_TEMP "fmf-cargo-sbom"
        cargo install cargo-sbom --locked --version 0.10.0 --root $cargoRoot
"#;

        let parity = ci_mirror_parity(
            &[
                (".github/workflows/fuzz.yml", workflow),
                (".github/actions/sbom-scan/action.yml", sbom_action),
                (
                    ".github/actions/setup-winapp/action.yml",
                    MIRROR_TEST_WINAPP_ACTION,
                ),
                (
                    ".github/actions/setup-actionlint/action.yml",
                    MIRROR_TEST_ACTIONLINT_ACTION,
                ),
            ],
            &mirror_test_pins(),
            &mirror_test_winapp_pin(),
        );

        assert!(parity.problems.is_empty(), "{:?}", parity.problems);
        assert_eq!(parity.compared, 16);
        assert_eq!(parity.excepted, 4);
    }

    #[test]
    fn actionlint_verifier_rejects_path_mutation() {
        let poisoned = MIRROR_TEST_ACTIONLINT_ACTION.replace("GITHUB_OUTPUT", "GITHUB_PATH");
        let ci = load_ci_yaml(".github/actions/setup-actionlint/action.yml", &poisoned)
            .expect("fixture parses");
        let mut parity = MirrorParity::default();
        compare_actionlint_action_pin(&mut parity, &mirror_test_pins(), &ci);
        let problems = parity.problems.join("\n");
        assert!(problems.contains("missing fail-closed verifier `$GITHUB_OUTPUT`"));
        assert!(problems.contains("absolute output, not PATH mutation"));
    }

    #[test]
    fn ci_mirror_parity_reports_drift_unpinned_and_unclassified_tools() {
        let workflow = r"
jobs:
  test:
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/setup-dotnet@immutable
        with:
          dotnet-version: 10.0.999
      - uses: microsoft/setup-WinAppCli@immutable
      - uses: taiki-e/install-action@immutable
        with:
          tool: just@1.53.0,cargo-audit,mystery@1.0.0
";

        let drifted_action = MIRROR_TEST_WINAPP_ACTION.replace("0.5.0", "0.4.0");
        let parity = ci_mirror_parity(
            &[
                (".github/workflows/broken.yml", workflow),
                (".github/actions/setup-winapp/action.yml", &drifted_action),
                (
                    ".github/actions/setup-actionlint/action.yml",
                    MIRROR_TEST_ACTIONLINT_ACTION,
                ),
            ],
            &mirror_test_pins(),
            &mirror_test_winapp_pin(),
        );
        let problems = parity.problems.join("\n");

        assert!(problems.contains("CI pins 10.0.999"));
        assert!(problems.contains("external setup-WinAppCli bypasses"));
        assert!(problems.contains("no ./.github/actions/setup-winapp wiring"));
        assert!(problems.contains("`WINAPP_VERSION` is 0.4.0"));
        assert!(problems.contains("CI pins 1.53.0"));
        assert!(problems.contains("`cargo-audit` is not version-pinned"));
        assert!(problems.contains("`mystery` has no mise mirror or explicit exception"));
    }

    #[test]
    fn every_ui_automation_workflow_wires_verified_winapp() {
        let common = r"
jobs:
  test:
    runs-on: windows-2025
    steps:
      - uses: actions/setup-dotnet@immutable
        with:
          dotnet-version: 10.0.302
      - uses: ./.github/actions/setup-winapp
        id: winapp
      - uses: ./.github/actions/setup-actionlint
      - uses: taiki-e/install-action@immutable
        with:
          tool: just@1.54.0
          fallback: none
      - run: just ui-test-published
        env:
          FMF_UI_CLI: ${{ steps.winapp.outputs.cli-path }}
";
        let missing = r"
jobs:
  test:
    runs-on: windows-2025
    steps:
      - run: just ui-test-published
";

        let parity = ci_mirror_parity(
            &[
                (".github/workflows/common.yml", common),
                (".github/workflows/missing.yml", missing),
                (
                    ".github/actions/setup-winapp/action.yml",
                    MIRROR_TEST_WINAPP_ACTION,
                ),
                (
                    ".github/actions/setup-actionlint/action.yml",
                    MIRROR_TEST_ACTIONLINT_ACTION,
                ),
            ],
            &mirror_test_pins(),
            &mirror_test_winapp_pin(),
        );

        assert!(parity
            .problems
            .iter()
            .any(|problem| problem.contains(
                ".github/workflows/missing.yml: runs UI automation without ./.github/actions/setup-winapp"
            )));
    }

    #[test]
    fn committed_ci_mirrors_match_mise_toml() {
        let mise_toml =
            std::fs::read_to_string(paths::mise_toml()).expect("read committed mise.toml");
        let check = ci_mirror_check_at(
            &paths::repo_root(),
            &parse_mise_pins(&mise_toml),
            &mise_toml,
        );

        assert_eq!(check.status, Status::Ok, "{}", check.detail);
    }

    #[test]
    fn version_satisfies_loose_pins() {
        assert!(version_satisfies("10", "10.0.118"));
        assert!(version_satisfies("1.95", "1.95.0"));
        assert!(version_satisfies("1.95", "1.95.3"));
        assert!(version_satisfies("1", "1.53.0"));
        assert!(version_satisfies("1.53.0", "1.53.0"));
        assert!(!version_satisfies("1.96", "1.95.0"));
        assert!(!version_satisfies("1.95", "1.9"));
        assert!(!version_satisfies("1.95.0", "1.95"));
    }

    #[test]
    fn pulls_version_token_from_tool_output() {
        assert_eq!(
            first_version_token("rustc 1.95.0 (abc 2026-01-01)"),
            Some("1.95.0")
        );
        assert_eq!(first_version_token("just 1.53.0"), Some("1.53.0"));
        assert_eq!(first_version_token("10.0.118"), Some("10.0.118"));
        assert_eq!(first_version_token("v24.4.0"), Some("24.4.0"));
        assert_eq!(first_version_token("no version here"), None);
    }

    #[test]
    fn overall_is_the_worst_status() {
        let ok = vec![Check::ok("a", ""), Check::info("b", "")];
        assert_eq!(overall(&ok), Status::Ok);
        let warn = vec![Check::ok("a", ""), Check::new(Status::Warn, "b", "")];
        assert_eq!(overall(&warn), Status::Warn);
        let fail = vec![Check::new(Status::Warn, "a", ""), Check::fail("b", "")];
        assert_eq!(overall(&fail), Status::Fail);
    }

    #[test]
    fn render_shows_each_status_tag() {
        let checks = vec![Check::ok("alpha", "fine"), Check::fail("beta", "broken")];
        let out = render(&checks);
        assert!(out.contains("[ OK ]"));
        assert!(out.contains("[FAIL]"));
        assert!(out.contains("alpha"));
        assert!(out.contains("broken"));
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("xtask-doctor-{tag}-{}", std::process::id()))
    }

    #[test]
    fn build_layout_allows_root_build_and_csharp_obj() {
        let root = scratch("layout-allowed");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("build").join("engine")).unwrap();
        std::fs::create_dir_all(root.join("app").join("FindMyFiles").join("obj")).unwrap();
        std::fs::create_dir_all(root.join("app").join("FindMyFiles.Tests").join("obj")).unwrap();

        let check = build_layout_check_at(&root);

        assert_eq!(check.status, Status::Ok);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_layout_detects_nested_build_directories_hidden_by_ignore_rule() {
        let root = scratch("layout-nested-build");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("docfx").join("build")).unwrap();
        let coverage = root
            .join("app")
            .join("FindMyFiles.Tests")
            .join("build")
            .join("cov.xml");
        std::fs::create_dir_all(coverage.parent().unwrap()).unwrap();
        std::fs::write(coverage, b"coverage").unwrap();

        let check = build_layout_check_at(&root);

        assert_eq!(check.status, Status::Fail);
        assert!(check.detail.contains("docfx/build"));
        assert!(check.detail.contains("app/FindMyFiles.Tests/build"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_layout_detects_app_local_outputs_but_not_obj() {
        let root = scratch("layout-app-output");
        let _ = std::fs::remove_dir_all(&root);
        let app = root.join("app").join("FindMyFiles");
        for name in [
            "Bin",
            "Debug",
            "Release",
            "artifacts",
            "Logs",
            "AppPackages",
            "BundleArtifacts",
            "publish",
            "PublishScripts",
            "Generated Files",
            "TestResults-2026",
        ] {
            std::fs::create_dir_all(app.join(name)).unwrap();
        }
        std::fs::create_dir_all(app.join("obj")).unwrap();

        let check = build_layout_check_at(&root);

        assert_eq!(check.status, Status::Fail);
        for name in [
            "Bin",
            "Debug",
            "Release",
            "artifacts",
            "Logs",
            "AppPackages",
            "BundleArtifacts",
            "publish",
            "PublishScripts",
            "Generated Files",
            "TestResults-2026",
        ] {
            assert!(
                check.detail.contains(&format!("app/FindMyFiles/{name}")),
                "missing {name} from {}",
                check.detail
            );
        }
        assert!(!check.detail.contains("app/FindMyFiles/obj"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
