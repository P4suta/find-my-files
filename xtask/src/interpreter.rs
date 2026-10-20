use std::collections::BTreeSet;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;

use anyhow::{ensure, Context, Result};

use crate::{cmd, paths};

const REQUIRED_MIRI_TESTS: [&str; 3] = [
    "volume::tests::decoding_rejects_invalid_utf8_and_stops_before_trailing_bytes",
    "volume::tests::every_drive_letter_roundtrips",
    "volume::tests::truncation_keeps_bytes_and_rejects_a_split_unicode_scalar",
];

pub fn miri() -> Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "run Miri on Linux x64"
    );
    ensure!(
        std::env::var_os("MIRIFLAGS").is_none_or(|flags| flags.is_empty()),
        "run Miri with its default isolation and borrow checks"
    );
    miri_suite("fmf-contract", "", &REQUIRED_MIRI_TESTS)?;
    miri_suite(
        "fmf-core",
        "wtf8::tests::",
        &[
            "wtf8::tests::malformed_sequences_preserve_the_output_prefix",
            "wtf8::tests::slice_decoding_preserves_rejected_output_and_unused_units",
        ],
    )?;
    miri_suite(
        "fmf-runtime",
        "read_vec_supplies_initialized_bytes_to_safe_readers",
        &["index::snapshot::tests::read_vec_supplies_initialized_bytes_to_safe_readers"],
    )
}

fn miri_suite(package: &str, filter: &str, required: &[&str]) -> Result<()> {
    let root = paths::repo_root();
    let args = [
        "miri",
        "test",
        "--locked",
        "--manifest-path",
        "engine/Cargo.toml",
        "--package",
        package,
        "--lib",
        "--target-dir",
    ];
    let target = paths::verification_dir().join("miri");
    let listing = Command::new("cargo")
        .current_dir(&root)
        .args(args)
        .arg(&target)
        .arg(filter)
        .args(["--", "--list"])
        .output()?;
    ensure!(
        listing.status.success(),
        "Miri test discovery failed: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let stdout = String::from_utf8(listing.stdout)?;
    let tests: BTreeSet<_> = stdout
        .lines()
        .filter_map(|line| line.strip_suffix(": test"))
        .collect();
    ensure!(
        !tests.is_empty() && required.iter().all(|name| tests.contains(name)),
        "Miri must execute the registered boundary tests for {package}"
    );
    let status = Command::new("cargo")
        .current_dir(&root)
        .args(args)
        .arg(&target)
        .arg(filter)
        .status()?;
    ensure!(status.success(), "Miri failed: {status}");
    Ok(())
}

pub fn fuzz_replay() -> Result<()> {
    fuzz_run(None)
}

pub fn fuzz(target: &str, seconds: NonZeroU32) -> Result<()> {
    fuzz_run(Some((target, seconds)))
}

fn fuzz_run(selection: Option<(&str, NonZeroU32)>) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "replay fuzz corpora on Linux x64"
    );
    let root = paths::repo_root();
    cmd::capture_stdout(
        &root,
        "cargo",
        &[
            "metadata",
            "--locked",
            "--manifest-path",
            "engine/fuzz/Cargo.toml",
            "--format-version",
            "1",
        ],
    )?;
    let lock_path = root.join("engine/fuzz/Cargo.lock");
    let lock = fs::read(&lock_path)?;
    let manifest = relocated_fuzz_manifest(
        &fs::read_to_string(root.join("engine/fuzz/Cargo.toml"))?,
        &root.join("engine/fuzz"),
    )?;
    let binaries = manifest["bin"]
        .as_array_of_tables()
        .context("missing fuzz targets")?;
    ensure!(!binaries.is_empty(), "empty fuzz target inventory");
    if let Some((target, _)) = selection {
        ensure!(
            binaries
                .iter()
                .any(|binary| binary["name"].as_str() == Some(target)),
            "unknown fuzz target"
        );
    }
    let workspace = paths::fuzz_workspace_dir();
    fs::create_dir_all(&workspace)?;
    fs::write(workspace.join("Cargo.toml"), manifest.to_string())?;
    let workspace_lock = workspace.join("Cargo.lock");
    fs::write(&workspace_lock, &lock)?;
    let workspace_path = workspace.to_str().context("invalid fuzz workspace path")?;
    for binary in binaries {
        let name = binary["name"]
            .as_str()
            .context("missing fuzz target name")?;
        ensure!(
            name.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "invalid fuzz target"
        );
        if selection.is_some_and(|(target, _)| target != name) {
            continue;
        }
        let corpus = root.join("engine/fuzz/corpus").join(name);
        let mut count = 0;
        for entry in fs::read_dir(&corpus).with_context(|| format!("missing corpus for {name}"))? {
            let entry = entry?;
            ensure!(
                entry.file_type()?.is_file() && entry.metadata()?.len() > 0,
                "invalid corpus entry {}",
                entry.path().display()
            );
            count += 1;
        }
        ensure!(count > 0, "empty corpus for {name}");
        let path = corpus.to_str().context("invalid corpus path")?;
        let artifacts = paths::fuzz_artifacts_dir().join(name);
        fs::create_dir_all(&artifacts)?;
        let artifact_prefix = format!("-artifact_prefix={}/", artifacts.display());
        let writable = paths::fuzz_corpus_dir().join(name);
        let base_args = [
            "fuzz",
            "run",
            name,
            "--target",
            "x86_64-unknown-linux-gnu",
            "--fuzz-dir",
            workspace_path,
        ];
        let mut replay_args = base_args.to_vec();
        replay_args.extend([path, "--", &artifact_prefix, "-runs=0", "-timeout=5"]);
        cmd::run(&root.join("engine"), "cargo", &replay_args)?;
        if let Some((_, seconds)) = selection {
            fs::create_dir_all(&writable)?;
            let mut args = base_args.to_vec();
            args.push(writable.to_str().context("invalid writable corpus path")?);
            let budget = format!("-max_total_time={seconds}");
            args.extend([path, "--", &artifact_prefix, &budget, "-timeout=5"]);
            cmd::run(&root.join("engine"), "cargo", &args)?;
        }
        ensure!(
            fs::read(&lock_path)? == lock && fs::read(&workspace_lock)? == lock,
            "fuzz changed a lockfile"
        );
    }
    Ok(())
}

fn relocated_fuzz_manifest(source: &str, directory: &Path) -> Result<toml_edit::DocumentMut> {
    ensure!(
        directory.is_absolute(),
        "fuzz source directory must be absolute"
    );
    let mut manifest = source.parse::<toml_edit::DocumentMut>()?;
    rebase_dependencies(manifest.as_table_mut(), directory)?;
    if let Some(targets) = manifest.get_mut("target") {
        for (_, target) in targets
            .as_table_like_mut()
            .context("invalid fuzz targets")?
            .iter_mut()
        {
            rebase_dependencies(
                target
                    .as_table_like_mut()
                    .context("invalid fuzz target configuration")?,
                directory,
            )?;
        }
    }
    let binaries = manifest
        .get_mut("bin")
        .context("missing fuzz targets")?
        .as_array_of_tables_mut()
        .context("missing fuzz targets")?;
    ensure!(!binaries.is_empty(), "empty fuzz target inventory");
    for binary in binaries.iter_mut() {
        rebase_path(
            binary
                .get_mut("path")
                .context("missing fuzz target source")?,
            directory,
        )?;
    }
    Ok(manifest)
}

fn rebase_dependencies(table: &mut dyn toml_edit::TableLike, directory: &Path) -> Result<()> {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = table.get_mut(section) {
            for (_, dependency) in dependencies
                .as_table_like_mut()
                .context("invalid fuzz dependencies")?
                .iter_mut()
            {
                if let Some(path) = dependency
                    .as_table_like_mut()
                    .and_then(|d| d.get_mut("path"))
                {
                    rebase_path(path, directory)?;
                }
            }
        }
    }
    Ok(())
}

fn rebase_path(value: &mut toml_edit::Item, directory: &Path) -> Result<()> {
    let path = directory.join(value.as_str().context("invalid fuzz source path")?);
    *value = toml_edit::value(path.to_str().context("invalid absolute fuzz source path")?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocated_fuzz_manifest_keeps_every_original_source_and_setting() {
        let directory = paths::repo_root().join("engine/fuzz");
        let source = include_str!("../../engine/fuzz/Cargo.toml");
        let original = source
            .parse::<toml_edit::DocumentMut>()
            .expect("fuzz manifest");
        let relocated = relocated_fuzz_manifest(source, &directory).expect("relocated manifest");
        for section in ["package", "profile", "workspace"] {
            assert_eq!(
                original[section].to_string(),
                relocated[section].to_string()
            );
        }
        let original_bins = original["bin"].as_array_of_tables().expect("fuzz binaries");
        let relocated_bins = relocated["bin"]
            .as_array_of_tables()
            .expect("relocated binaries");
        assert_eq!(original_bins.len(), relocated_bins.len());
        for (original, relocated) in original_bins.iter().zip(relocated_bins) {
            let original_path = directory.join(original["path"].as_str().expect("source path"));
            let relocated_path = Path::new(relocated["path"].as_str().expect("relocated source"));
            assert!(relocated_path.is_absolute());
            assert_eq!(
                fs::canonicalize(original_path).expect("original source"),
                fs::canonicalize(relocated_path).expect("actual source")
            );
            for field in ["name", "test", "doc", "bench"] {
                assert_eq!(original[field].to_string(), relocated[field].to_string());
            }
        }
        for (name, dependency) in original["dependencies"].as_table().expect("dependencies") {
            if let Some(path) = dependency.get("path").and_then(toml_edit::Item::as_str) {
                let actual = Path::new(
                    relocated["dependencies"][name]["path"]
                        .as_str()
                        .expect("dependency path"),
                );
                assert!(actual.is_absolute());
                assert_eq!(
                    fs::canonicalize(directory.join(path)).expect("original crate"),
                    fs::canonicalize(actual).expect("actual crate")
                );
            } else {
                assert_eq!(
                    dependency.to_string(),
                    relocated["dependencies"][name].to_string()
                );
            }
        }
        assert!(paths::fuzz_workspace_dir().starts_with(paths::build_root()));
        assert!(!paths::fuzz_workspace_dir().starts_with(directory));
    }

    #[test]
    fn relocation_preserves_target_features_and_registry_dependencies() {
        let source = r#"
[dependencies]
registry = { version = "1", features = ["feature"] }
local = { path = "../local", default-features = false }
[target.'cfg(unix)'.build-dependencies]
helper = { path = "../helper", features = ["build"] }
[[bin]]
name = "decode"
path = "fuzz_targets/decode.rs"
"#;
        let directory = paths::repo_root().join("engine/fuzz");
        let relocated = relocated_fuzz_manifest(source, &directory).expect("relocated manifest");
        assert_eq!(
            relocated["dependencies"]["registry"]["version"].as_str(),
            Some("1")
        );
        assert_eq!(
            relocated["dependencies"]["registry"]["features"][0].as_str(),
            Some("feature")
        );
        assert_eq!(
            relocated["dependencies"]["local"]["default-features"].as_bool(),
            Some(false)
        );
        let helper = &relocated["target"]["cfg(unix)"]["build-dependencies"]["helper"];
        assert_eq!(helper["features"][0].as_str(), Some("build"));
        assert_eq!(
            Path::new(helper["path"].as_str().expect("helper path")),
            directory.join("../helper")
        );
    }

    #[test]
    fn relocation_refuses_ambiguous_or_missing_source_paths() {
        let directory = paths::repo_root().join("engine/fuzz");
        assert!(relocated_fuzz_manifest("", &directory).is_err());
        assert!(relocated_fuzz_manifest("[[bin]]\nname = 'missing'", &directory).is_err());
        assert!(relocated_fuzz_manifest("[[bin]]\npath = 1", &directory).is_err());
        assert!(relocated_fuzz_manifest("bin = []", &directory).is_err());
        assert!(relocated_fuzz_manifest(
            include_str!("../../engine/fuzz/Cargo.toml"),
            Path::new("relative")
        )
        .is_err());
    }

    #[test]
    fn fuzz_crashes_are_collected_from_the_actual_output_directory() {
        let workflow =
            yaml_rust2::YamlLoader::load_from_str(include_str!("../../.github/workflows/fuzz.yml"))
                .expect("fuzz workflow");
        let upload = workflow[0]["jobs"]["fuzz"]["steps"]
            .as_vec()
            .expect("fuzz steps")
            .iter()
            .find(|step| {
                step["uses"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("actions/upload-artifact@"))
            })
            .expect("crash upload");
        assert_eq!(
            paths::repo_root().join(upload["with"]["path"].as_str().expect("crash path")),
            paths::fuzz_artifacts_dir()
        );
        assert_eq!(upload["if"].as_str(), Some("failure()"));
    }
}
