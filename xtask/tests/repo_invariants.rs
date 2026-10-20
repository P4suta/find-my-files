//! Executable tripwires for the repository invariants that AGENTS.md and the ADRs otherwise state only in prose.
//!
//! A rule with no gate is a rule that is already half-broken: every invariant below has a failure mode that is silent (an analyzer set that stops being injected, a toolchain that stops being owned by mise, a trait seam that quietly becomes a third port).
//! These tests read the committed tree as text — the same structural style as the workflow guards beside them — so the prose and the repository cannot drift apart unnoticed.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const ENGINE_MANIFEST: &str = include_str!("../../engine/Cargo.toml");
const XTASK_MANIFEST: &str = include_str!("../Cargo.toml");
const FFI_MANIFEST: &str = include_str!("../../engine/crates/fmf-ffi/Cargo.toml");
const CONTRACT_MANIFEST: &str = include_str!("../../engine/crates/fmf-contract/Cargo.toml");
const NATIVE_ENGINE: &str = include_str!("../../app/FindMyFiles/Engine/NativeEngine.cs");
const APP_CSPROJ: &str = include_str!("../../app/FindMyFiles/FindMyFiles.csproj");
const QUERY_MOD: &str = include_str!("../../engine/crates/fmf-runtime/src/query/mod.rs");
const JUSTFILE: &str = include_str!("../../justfile");
const LEFTHOOK: &str = include_str!("../../lefthook.yml");
const DEFERRED: &str = include_str!("../../engine/crates/fmf-runtime/src/scan/deferred.rs");
const USN_SESSION: &str = include_str!("../../engine/crates/fmf-runtime/src/usn/session.rs");

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has the repository as its parent")
        .to_path_buf()
}

/// Walk the working tree the way `MSBuild`, rustup and the .NET SDK do — by directory scan, not through the git index — so an *untracked* stray file is caught as well.
/// `build/` is the single generated-output tree (ADR-0021) and `.git/` is not source, so neither can hide or fake a violation.
fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", directory.display()));
        for entry in entries {
            let entry = entry.expect("directory entry must be readable");
            let kind = entry.file_type().expect("file type must be readable");
            let path = entry.path();
            if kind.is_dir() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !matches!(name.as_str(), ".git" | "build" | "target") {
                    pending.push(path);
                }
            } else if kind.is_file() {
                files.push(path);
            }
        }
    }
    files
}

/// Repository-relative, forward-slashed paths of every file whose name matches one of `names`.
/// The comparison is ASCII-case-insensitive because the tools that consume these files resolve them case-insensitively on Windows: a `directory.build.props` would be just as effective as the canonical spelling.
fn files_named(names: &[&str]) -> Vec<String> {
    let root = repo();
    let mut found: Vec<String> = source_files(&root)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    names
                        .iter()
                        .any(|candidate| name.eq_ignore_ascii_case(candidate))
                })
        })
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(&path)
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect();
    found.sort();
    found
}

/// The body of a TOML table: everything between its header and the next one.
fn toml_section<'a>(manifest: &'a str, header: &str) -> Option<&'a str> {
    let start = manifest.find(header)? + header.len();
    let body = &manifest[start..];
    Some(body.find("\n[").map_or(body, |offset| &body[..offset]))
}

/// A TOML line is meaningful unless it is blank or a whole-line comment.
fn has_meaningful_line(section: &str) -> bool {
    section
        .lines()
        .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
}

#[test]
fn no_msbuild_directory_level_import_files_exist() {
    let strays = files_named(&[
        "Directory.Build.props",
        "Directory.Build.targets",
        "Directory.Packages.props",
    ]);

    assert!(
        strays.is_empty(),
        "MSBuild directory-level import files silently disable the analyzer injection; delete {strays:?}"
    );
}

#[test]
fn toolchain_selection_belongs_to_mise_alone() {
    let strays = files_named(&["rust-toolchain.toml", "rust-toolchain", "global.json"]);

    assert!(
        strays.is_empty(),
        "the toolchain is pinned by mise.toml alone; delete {strays:?}"
    );
}

#[test]
fn xtask_is_its_own_workspace_and_never_an_engine_member() {
    let engine = ENGINE_MANIFEST
        .parse::<toml_edit::DocumentMut>()
        .expect("valid engine manifest");
    let members = engine["workspace"]["members"]
        .as_array()
        .expect("engine workspace members");
    for member in members {
        let member = member.as_str().expect("workspace member path");
        assert!(
            !member.contains("xtask"),
            "engine workspace must not list xtask as a member: {member}"
        );
        assert!(
            !member.contains(".."),
            "engine workspace members must stay inside engine/: {member}"
        );
    }
    let xtask = XTASK_MANIFEST
        .parse::<toml_edit::DocumentMut>()
        .expect("valid xtask manifest");
    let workspace = xtask["workspace"]
        .as_table()
        .expect("standalone xtask workspace");
    assert!(
        !workspace.contains_key("members"),
        "xtask is a single-crate workspace; it must not grow a members list"
    );
}

#[test]
fn the_shipped_engine_dll_has_one_name_in_all_three_places() {
    let library = toml_section(FFI_MANIFEST, "\n[lib]").expect("fmf-ffi must declare [lib]");
    let name = library
        .lines()
        .find_map(|line| line.trim().strip_prefix("name = "))
        .map(|value| value.trim().trim_matches('"'))
        .expect("fmf-ffi [lib] must set an explicit name");
    assert_eq!(
        name, "fmf_engine",
        "the shipped DLL name is frozen; changing it breaks every consumer at runtime"
    );

    let import = format!("[LibraryImport(\"{name}\"");
    let engine_imports = NATIVE_ENGINE.matches("[LibraryImport(\"").count();
    assert!(
        engine_imports > 0,
        "NativeEngine.cs must bind the engine DLL"
    );
    assert_eq!(
        NATIVE_ENGINE.matches(import.as_str()).count(),
        engine_imports,
        "every P/Invoke in NativeEngine.cs must target `{name}`, not a second library"
    );

    assert!(
        APP_CSPROJ.contains(&format!(
            "..\\..\\build\\engine\\release\\{name}.dll\" Link=\"{name}.dll\""
        )),
        "the csproj must copy the engine payload under its exact `{name}.dll` name"
    );
}

#[test]
fn the_contract_crate_stays_a_dependency_free_leaf() {
    let dependencies = toml_section(CONTRACT_MANIFEST, "\n[dependencies]")
        .expect("fmf-contract must keep an explicit, empty [dependencies] table");
    assert!(
        !has_meaningful_line(dependencies),
        "fmf-contract must stay dependency-free, found: {dependencies}"
    );

    for table in ["[dev-dependencies]", "[build-dependencies]", "[target."] {
        assert!(
            !CONTRACT_MANIFEST.contains(table),
            "fmf-contract must not declare `{table}`; it is a std-only leaf crate"
        );
    }
}

const EXPECTED_CORE_PUB_TRAITS: &[(&str, &str)] = &[
    ("src/engine/seams.rs", "JournalSource"),
    ("src/engine/seams.rs", "SnapshotStore"),
    ("src/query/dates.rs", "DateResolver"),
];

#[test]
fn fmf_runtime_exposes_exactly_the_two_capped_trait_seams() {
    let core = repo().join("engine").join("crates").join("fmf-runtime");
    let mut actual = BTreeSet::new();
    for path in source_files(&core) {
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            continue;
        }
        let relative = path
            .strip_prefix(&core)
            .expect("walked path is under fmf-runtime")
            .display()
            .to_string()
            .replace('\\', "/");
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        for line in source.lines() {
            let Some(declaration) = line.trim_start().strip_prefix("pub trait ") else {
                continue;
            };
            let name = declaration
                .split(|character: char| {
                    character.is_whitespace() || matches!(character, ':' | '<' | '{')
                })
                .next()
                .expect("split always yields a first element");
            actual.insert((relative.clone(), name.to_owned()));
        }
    }

    let expected: BTreeSet<_> = EXPECTED_CORE_PUB_TRAITS
        .iter()
        .map(|(file, name)| ((*file).to_owned(), (*name).to_owned()))
        .collect();
    assert_eq!(
        actual, expected,
        "fmf-runtime's trait seams are capped at SnapshotStore + JournalSource (ADR-0018); \
         a new port needs an ADR, not a new trait"
    );
    assert!(
        QUERY_MOD.contains("pub(crate) mod dates;"),
        "query::dates must stay crate-private, or DateResolver becomes a third public seam"
    );
}

const POWERSHELL_ONLY_TOKENS: &[&str] = &[
    "powershell.exe",
    "$env:",
    "$PSScriptRoot",
    "Get-",
    "Set-",
    "New-Item",
    "Remove-Item",
    "Copy-Item",
    "Start-Process",
    "Test-Path",
    "Join-Path",
    "Write-Host",
    "Write-Output",
    "Select-Object",
    "Where-Object",
    "ForEach-Object",
    "ConvertFrom-Json",
    "ConvertTo-Json",
    "Out-File",
    "-ErrorAction",
];

const POSIX_ONLY_TOKENS: &[&str] = &[
    "/dev/null",
    "export ",
    "&&",
    "||",
    "$(",
    "`",
    "<<'",
    "2>/dev",
];

/// Lines that a shell actually executes: prose comments and the `windows-shell` declaration are excluded.
/// The declaration names the *launching interpreter*,
/// which is an implementation detail of `just` on Windows — the rule is about the definitions themselves, so excluding it is part of the rule, not a hole.
fn executable_lines(source: &str) -> impl Iterator<Item = (usize, &str)> {
    source
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
        .filter(|(_, line)| {
            let trimmed = line.trim_start();
            !trimmed.is_empty()
                && !trimmed.starts_with('#')
                && !trimmed.starts_with("set windows-shell")
        })
}

#[test]
fn task_definitions_never_depend_on_one_shell() {
    for (name, source) in [("justfile", JUSTFILE), ("lefthook.yml", LEFTHOOK)] {
        for (number, line) in executable_lines(source) {
            for token in POWERSHELL_ONLY_TOKENS.iter().chain(POSIX_ONLY_TOKENS) {
                assert!(
                    !line.contains(token),
                    "{name}:{number} uses shell-specific syntax `{token}`: {line}"
                );
            }
        }
    }
}

#[test]
fn raw_volume_streams_are_read_through_the_sector_aligned_adapter() {
    for (name, source) in [
        ("scan/deferred.rs", DEFERRED),
        ("usn/session.rs", USN_SESSION),
    ] {
        let calls = source.matches("visit_list_stream(").count();
        let wrapped = source.matches("SectorAlignedReader::new(").count();
        assert!(
            calls > 0,
            "{name} is pinned here because it streams attribute lists; \
             if that moved, move this guard with it"
        );
        assert_eq!(
            calls, wrapped,
            "{name} calls visit_list_stream {calls} time(s) but wraps a reader \
             in SectorAlignedReader {wrapped} time(s); every raw-volume stream \
             read must go through the adapter"
        );
    }
}
