//! Deterministic, artifact-derived `CycloneDX` 1.6 SBOM generation.
//!
//! `cargo-sbom` and `NuGet` remain the dependency resolvers, but neither generated
//! document is publishable as-is:
//!
//! - cargo-sbom 0.10 wraps the selected package under
//!   `metadata.component.components[0]`, and emits a different random/timestamped
//!   document for every entry point;
//! - a project-level .NET SBOM describes build/analyzer dependencies and misses
//!   self-contained runtime payloads.
//!
//! This module treats the final distribution tree as the shipping truth. It
//! validates and merges the three raw Rust graphs, then reconciles every shipped
//! .NET/WinAppSDK file with `FindMyFiles.deps.json`, `project.assets.json`, and
//! the exact restored `NuGet` package archives. All reference handling and graph
//! checks are pure functions with unit tests; [`run`] is only the filesystem
//! adapter.

use crate::{
    bundle_seal::{self, BundleState},
    checksum, fsx, paths, prune, publish,
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use zip::ZipArchive;

const CYCLONEDX_SCHEMA: &str = "https://cyclonedx.org/schema/bom-1.6.schema.json";
const CARGO_RAW_FILES: [(&str, &str); 3] = [
    ("fmf-service", "fmf-service.cdx.json"),
    ("fmf-ffi", "fmf-ffi.cdx.json"),
    ("fmf-launcher", "fmf-launcher.cdx.json"),
];
const APP_ROOT_NAME: &str = "FindMyFiles";
const ENGINE_ROOT_NAME: &str = "fmf-engine";
const NUGET_SOURCE: &str = "https://api.nuget.org/v3/index.json";
const CLR_DIRECTORY_INDEX: usize = 14;

/// Crates that belong to developer/test binaries, never the shipped service,
/// FFI library, or launcher. cargo-sbom currently filters non-normal edges; the
/// explicit denylist makes that behavior a checked contract rather than an
/// undocumented assumption.
const DEVELOPER_ONLY_CRATES: &[&str] = &[
    "cargo-fuzz",
    "clap_complete",
    "criterion",
    "fmf-cli",
    "indicatif",
    "libfuzzer-sys",
    "proptest",
    "test-case",
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileRecord {
    path: String,
    sha256: String,
    size: u64,
    is_pe: bool,
    authenticode_payload_sha256: Option<String>,
    managed_metadata_sha256: Option<String>,
    file_version: Option<FileVersion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceFile {
    path: String,
    sha256: String,
    size: u64,
    is_pe: bool,
    managed_metadata_sha256: Option<String>,
    file_version: Option<FileVersion>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FileVersion([u16; 4]);

impl fmt::Display for FileVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, build, revision] = self.0;
        write!(formatter, "{major}.{minor}.{build}.{revision}")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageInventory {
    id: String,
    content_hash: String,
    files: Vec<SourceFile>,
}

#[derive(Clone, Debug)]
struct RawCargoGraph {
    root_ref: String,
    root_component: Value,
    components: BTreeMap<String, Value>,
    edges: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DotnetKind {
    Package,
    RuntimePack,
    Reference,
}

#[derive(Clone, Debug)]
struct RuntimeAsset {
    source_path: String,
    output_path: String,
    transformable: bool,
    declared_file_version: Option<FileVersion>,
    has_declared_assembly_version: bool,
}

#[derive(Clone, Debug)]
struct DotnetNode {
    id: String,
    version: String,
    component_name: String,
    kind: DotnetKind,
    cache_path: Option<String>,
    content_hash: Option<String>,
    dependencies: BTreeSet<String>,
    assets: Vec<RuntimeAsset>,
}

#[derive(Clone, Debug)]
struct DotnetGraph {
    root_id: String,
    root_dependencies: BTreeSet<String>,
    nodes: BTreeMap<String, DotnetNode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerKind {
    App,
    Rust,
    Package,
}

#[derive(Clone, Debug)]
struct Assignment {
    owner: OwnerKind,
    component_id: Option<String>,
    source: Option<SourceFile>,
    transformed: bool,
}

pub fn run(version: &str, cargo_raw_dir: &Path) -> Result<()> {
    ensure!(
        !version.trim().is_empty(),
        "SBOM product version must not be blank"
    );
    ensure_exact_raw_files(cargo_raw_dir)?;

    let dist_files = collect_dist_files(&paths::dist_dir())?;
    let rust_files = rust_file_evidence(&dist_files)?;

    let mut raw_boms = Vec::with_capacity(CARGO_RAW_FILES.len());
    for (package, filename) in CARGO_RAW_FILES {
        let path = cargo_raw_dir.join(filename);
        let body = fs::read_to_string(&path)
            .with_context(|| format!("read cargo-sbom output {}", path.display()))?;
        let value = parse_json_strict(&body)
            .with_context(|| format!("parse cargo-sbom output {}", path.display()))?;
        raw_boms.push((package, value));
    }
    let borrowed_raw: Vec<(&str, &Value)> = raw_boms
        .iter()
        .map(|(name, value)| (*name, value))
        .collect();
    let engine_bom = merge_cargo_boms(version, &borrowed_raw, &rust_files)?;

    let deps_path = paths::app_dir().join("FindMyFiles.deps.json");
    let assets_path = paths::app_project_assets();
    let deps = read_json(&deps_path)?;
    let assets = read_json(&assets_path)?;
    let graph = parse_dotnet_graph(&deps, &assets)?;
    let inventories = load_package_inventories(&graph, &assets)?;
    let app_bom = build_app_bom(version, &graph, &inventories, &dist_files)?;

    let output_dir = paths::sbom_dir();
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("create SBOM output directory {}", output_dir.display()))?;
    preflight_final_bom_dir(&output_dir)?;
    write_json_atomic(&output_dir.join("fmf-engine.cdx.json"), &engine_bom)?;
    write_json_atomic(&output_dir.join("app.cdx.json"), &app_bom)?;
    verify_final_pair_at(&output_dir, version)?;

    println!(
        "Generated deterministic CycloneDX 1.6 SBOMs in {}",
        output_dir.display()
    );
    Ok(())
}

/// Verify that the canonical SBOM output contains exactly the two final,
/// structurally valid documents for `version` and no other filesystem entry.
pub fn verify_final_pair(version: &str) -> Result<()> {
    verify_final_pair_at(&paths::sbom_dir(), version)
}

fn read_json(path: &Path) -> Result<Value> {
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_json_strict(&body).with_context(|| format!("parse JSON {}", path.display()))
}

#[derive(Clone, Copy)]
struct StrictJsonSeed;

impl<'de> DeserializeSeed<'de> for StrictJsonSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an unambiguous JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictJsonSeed.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or_default());
        while let Some(value) = sequence.next_element_seed(StrictJsonSeed)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> std::result::Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::with_capacity(object.size_hint().unwrap_or_default());
        while let Some(key) = object.next_key::<String>()? {
            let value = object.next_value_seed(StrictJsonSeed)?;
            if values.insert(key.clone(), value).is_some() {
                return Err(A::Error::custom(format!(
                    "duplicate JSON object key `{key}`"
                )));
            }
        }
        Ok(Value::Object(values))
    }
}

fn parse_json_strict(body: &str) -> std::result::Result<Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(body);
    let value = StrictJsonSeed.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

fn ensure_exact_raw_files(dir: &Path) -> Result<()> {
    let root_metadata = fs::symlink_metadata(dir)
        .with_context(|| format!("inspect cargo-sbom raw directory {}", dir.display()))?;
    ensure!(
        root_metadata.is_dir() && !fsx::is_reparse_point(&root_metadata),
        "cargo-sbom raw path is not a real directory: {}",
        dir.display()
    );
    let expected: BTreeSet<String> = CARGO_RAW_FILES
        .iter()
        .map(|(_, filename)| (*filename).to_owned())
        .collect();
    let mut actual = BTreeSet::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        ensure!(
            metadata.is_file() && !fsx::is_reparse_point(&metadata),
            "cargo-sbom raw directory contains a non-file or reparse entry: {}",
            entry.path().display()
        );
        actual.insert(entry.file_name().to_string_lossy().into_owned());
    }
    ensure!(
        actual == expected,
        "expected exactly cargo-sbom raw files {expected:?}, found {actual:?}"
    );
    Ok(())
}

fn preflight_final_bom_dir(dir: &Path) -> Result<()> {
    let root_metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", dir.display()));
        }
    };
    ensure!(
        root_metadata.is_dir() && !fsx::is_reparse_point(&root_metadata),
        "SBOM output root must be a real directory: {}",
        dir.display()
    );
    let allowed = BTreeSet::from(["app.cdx.json", "fmf-engine.cdx.json"]);
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        ensure!(
            metadata.is_file() && !fsx::is_reparse_point(&metadata),
            "SBOM output contains a non-file or reparse entry: {}",
            entry.path().display()
        );
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("SBOM output filename is not Unicode"))?;
        ensure!(
            allowed.contains(name.as_str()),
            "unexpected stale SBOM output {name} — refusing an ambiguous artifact set"
        );
    }
    Ok(())
}

fn ensure_exact_final_boms(dir: &Path) -> Result<()> {
    preflight_final_bom_dir(dir)?;
    let expected = BTreeSet::from(["app.cdx.json", "fmf-engine.cdx.json"]);
    let actual = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .map(|entry| {
            entry?
                .file_name()
                .into_string()
                .map_err(|_| std::io::Error::other("SBOM output filename is not Unicode"))
        })
        .collect::<std::io::Result<BTreeSet<_>>>()?;
    ensure!(
        actual.iter().map(String::as_str).collect::<BTreeSet<_>>() == expected,
        "expected exactly app.cdx.json and fmf-engine.cdx.json, found {actual:?}"
    );
    Ok(())
}

fn verify_final_pair_at(dir: &Path, version: &str) -> Result<()> {
    ensure_exact_final_boms(dir)?;
    let engine = read_json(&dir.join("fmf-engine.cdx.json"))?;
    let app = read_json(&dir.join("app.cdx.json"))?;
    validate_final_bom(&engine, ENGINE_ROOT_NAME, version)?;
    validate_final_bom(&app, APP_ROOT_NAME, version)?;
    Ok(())
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(value).context("serialize deterministic SBOM")?;
    body.push(b'\n');
    fsx::write_file_atomic(path, &body)
        .with_context(|| format!("atomically write {}", path.display()))
}

fn collect_dist_files(root: &Path) -> Result<Vec<FileRecord>> {
    let identities = bundle_seal::collect_bundle_files(root, BundleState::Unsigned)?;
    let mut records = Vec::with_capacity(identities.len());
    for identity in identities {
        ensure!(
            identity.size != 0,
            "distribution contains an empty file: {}",
            identity.path
        );
        let path = join_safe_relative(root, &identity.path)?;
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        ensure!(
            bytes.len() as u64 == identity.size && checksum::sha256_hex(&bytes) == identity.sha256,
            "distribution file changed after bundle validation: {}",
            identity.path
        );
        let managed_metadata_sha256 = if identity.is_pe {
            managed_metadata_sha256(&bytes)
                .with_context(|| format!("inspect managed metadata in {}", identity.path))?
        } else {
            None
        };
        let file_version = if identity.is_pe {
            file_version_from_path(&path)
                .with_context(|| format!("read FileVersion from {}", identity.path))?
        } else {
            None
        };
        records.push(FileRecord {
            path: identity.path,
            sha256: identity.sha256,
            size: identity.size,
            is_pe: identity.is_pe,
            authenticode_payload_sha256: identity.pe_payload_sha256,
            managed_metadata_sha256,
            file_version,
        });
    }
    Ok(records)
}

fn normalize_archive_path(path: &str) -> Result<String> {
    let replaced = path.replace('\\', "/");
    ensure!(!replaced.is_empty(), "relative path must not be empty");
    ensure!(
        !replaced.starts_with('/') && !replaced.contains(':'),
        "path must be relative: {path}"
    );
    let mut parts = Vec::new();
    for part in replaced.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "path contains an unsafe segment: {path}"
        );
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn parse_file_version(value: &str, label: &str) -> Result<FileVersion> {
    let components = value
        .split('.')
        .map(|component| {
            ensure!(
                !component.is_empty()
                    && component.bytes().all(|byte| byte.is_ascii_digit())
                    && (component == "0" || !component.starts_with('0')),
                "{label} has a non-canonical FileVersion component: {value}"
            );
            component
                .parse::<u16>()
                .with_context(|| format!("{label} FileVersion component is outside u16: {value}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let components: [u16; 4] = components.try_into().map_err(|components: Vec<u16>| {
        anyhow!(
            "{label} FileVersion must have exactly four components, found {}: {value}",
            components.len()
        )
    })?;
    Ok(FileVersion(components))
}

#[derive(Clone, Copy)]
struct PeSection {
    virtual_address: usize,
    virtual_size: usize,
    raw_offset: usize,
    raw_size: usize,
}

/// Hash the exact CLR metadata directory embedded in a managed PE. `ReadyToRun`
/// is allowed to change native layout, but not assembly metadata identity.
fn managed_metadata_sha256(bytes: &[u8]) -> Result<Option<String>> {
    if !bytes.starts_with(b"MZ") {
        return Ok(None);
    }
    ensure!(bytes.len() >= 0x40, "PE is shorter than its DOS header");
    let pe_offset = sbom_read_u32(bytes, 0x3c, "DOS e_lfanew")? as usize;
    ensure!(
        bytes.get(pe_offset..pe_offset.saturating_add(4)) == Some(b"PE\0\0".as_slice()),
        "PE signature is missing or truncated"
    );
    let coff = pe_offset.checked_add(4).context("COFF offset overflow")?;
    let optional = coff
        .checked_add(20)
        .context("optional-header offset overflow")?;
    let section_count = usize::from(sbom_read_u16(bytes, coff + 2, "NumberOfSections")?);
    let optional_size = usize::from(sbom_read_u16(bytes, coff + 16, "SizeOfOptionalHeader")?);
    let optional_end = optional
        .checked_add(optional_size)
        .context("optional-header extent overflow")?;
    ensure!(optional_end <= bytes.len(), "truncated PE optional header");

    let magic = sbom_read_u16(bytes, optional, "optional-header magic")?;
    let (directory_count_offset, directories_offset) = match magic {
        0x010b => (92_usize, 96_usize),
        0x020b => (108_usize, 112_usize),
        _ => bail!("unsupported PE optional-header magic 0x{magic:04x}"),
    };
    let directory_count = sbom_read_u32(
        bytes,
        optional + directory_count_offset,
        "NumberOfRvaAndSizes",
    )? as usize;
    if directory_count <= CLR_DIRECTORY_INDEX {
        return Ok(None);
    }
    let clr_directory = optional
        .checked_add(directories_offset)
        .and_then(|offset| offset.checked_add(CLR_DIRECTORY_INDEX * 8))
        .context("CLR directory offset overflow")?;
    ensure!(
        clr_directory + 8 <= optional_end,
        "optional header truncates the declared CLR directory"
    );
    let clr_rva = sbom_read_u32(bytes, clr_directory, "CLR header RVA")? as usize;
    let clr_size = sbom_read_u32(bytes, clr_directory + 4, "CLR header size")? as usize;
    match (clr_rva, clr_size) {
        (0, 0) => return Ok(None),
        (0, _) | (_, 0) => bail!("CLR directory has an inconsistent zero RVA/size"),
        _ => {}
    }
    ensure!(
        clr_size >= 0x48,
        "CLR header is smaller than IMAGE_COR20_HEADER"
    );

    let size_of_headers = sbom_read_u32(bytes, optional + 60, "SizeOfHeaders")? as usize;
    let section_table_size = section_count
        .checked_mul(40)
        .context("section table size overflow")?;
    ensure!(
        optional_end
            .checked_add(section_table_size)
            .is_some_and(|end| end <= bytes.len() && end <= size_of_headers),
        "PE section table is truncated or outside SizeOfHeaders"
    );
    let mut sections = Vec::with_capacity(section_count);
    for index in 0..section_count {
        let section = optional_end + index * 40;
        sections.push(PeSection {
            virtual_size: sbom_read_u32(bytes, section + 8, "section VirtualSize")? as usize,
            virtual_address: sbom_read_u32(bytes, section + 12, "section VirtualAddress")? as usize,
            raw_size: sbom_read_u32(bytes, section + 16, "section SizeOfRawData")? as usize,
            raw_offset: sbom_read_u32(bytes, section + 20, "section PointerToRawData")? as usize,
        });
    }
    let clr_offset = rva_file_offset(
        bytes,
        clr_rva,
        clr_size,
        size_of_headers,
        &sections,
        "CLR header",
    )?;
    let metadata_rva = sbom_read_u32(bytes, clr_offset + 8, "CLR metadata RVA")? as usize;
    let metadata_size = sbom_read_u32(bytes, clr_offset + 12, "CLR metadata size")? as usize;
    ensure!(
        metadata_rva != 0 && metadata_size >= 20,
        "CLR header has an empty metadata directory"
    );
    let metadata_offset = rva_file_offset(
        bytes,
        metadata_rva,
        metadata_size,
        size_of_headers,
        &sections,
        "CLR metadata",
    )?;
    let metadata_end = metadata_offset
        .checked_add(metadata_size)
        .context("CLR metadata extent overflow")?;
    let metadata = &bytes[metadata_offset..metadata_end];
    Ok(Some(metadata_identity_sha256(metadata)?))
}

fn rva_file_offset(
    bytes: &[u8],
    rva: usize,
    size: usize,
    size_of_headers: usize,
    sections: &[PeSection],
    label: &str,
) -> Result<usize> {
    let end_rva = rva.checked_add(size).context("RVA extent overflow")?;
    if rva < size_of_headers && end_rva <= size_of_headers && end_rva <= bytes.len() {
        return Ok(rva);
    }
    let mut matches = sections.iter().filter_map(|section| {
        let span = section.virtual_size.max(section.raw_size);
        let section_end = section.virtual_address.checked_add(span)?;
        if rva < section.virtual_address || end_rva > section_end {
            return None;
        }
        let delta = rva - section.virtual_address;
        if delta.checked_add(size)? > section.raw_size {
            return None;
        }
        let offset = section.raw_offset.checked_add(delta)?;
        (offset.checked_add(size)? <= bytes.len()).then_some(offset)
    });
    let offset = matches
        .next()
        .ok_or_else(|| anyhow!("{label} RVA does not map to raw PE data"))?;
    ensure!(
        matches.next().is_none(),
        "{label} RVA maps to multiple PE sections"
    );
    Ok(offset)
}

fn metadata_identity_sha256(metadata: &[u8]) -> Result<String> {
    ensure!(
        sbom_read_u32(metadata, 0, "CLR metadata signature")? == 0x424a_5342,
        "CLR metadata has no BSJB signature"
    );
    let version_length = sbom_read_u32(metadata, 12, "CLR metadata version length")? as usize;
    ensure!(version_length != 0, "CLR metadata version string is empty");
    let version_end = 16_usize
        .checked_add(version_length)
        .context("CLR metadata version extent overflow")?;
    ensure!(
        version_end <= metadata.len(),
        "CLR metadata version string is truncated"
    );
    ensure!(
        metadata[16..version_end].contains(&0),
        "CLR metadata version string is not NUL-terminated"
    );
    let version_nul = metadata[16..version_end]
        .iter()
        .position(|byte| *byte == 0)
        .context("CLR metadata version string is not NUL-terminated")?;
    let version = &metadata[16..16 + version_nul];
    let storage_header = version_end
        .checked_add(3)
        .map(|value| value & !3)
        .context("CLR metadata alignment overflow")?;
    let stream_count = usize::from(sbom_read_u16(
        metadata,
        storage_header + 2,
        "CLR metadata stream count",
    )?);
    ensure!(
        stream_count != 0 && stream_count <= 32,
        "CLR metadata stream count is invalid: {stream_count}"
    );
    let mut cursor = storage_header + 4;
    let mut names = BTreeSet::new();
    let mut streams = Vec::with_capacity(stream_count);
    for _ in 0..stream_count {
        let offset = sbom_read_u32(metadata, cursor, "CLR stream offset")? as usize;
        let size = sbom_read_u32(metadata, cursor + 4, "CLR stream size")? as usize;
        let name_start = cursor + 8;
        let name_end = metadata
            .get(name_start..)
            .and_then(|tail| tail.iter().position(|byte| *byte == 0))
            .map(|relative| name_start + relative)
            .context("CLR stream name is not NUL-terminated")?;
        ensure!(
            name_end - name_start <= 32,
            "CLR stream name exceeds the metadata limit"
        );
        let name = std::str::from_utf8(&metadata[name_start..name_end])
            .context("CLR stream name is not UTF-8")?;
        ensure!(
            names.insert(name.to_owned()),
            "CLR metadata repeats stream {name}"
        );
        ensure!(
            offset
                .checked_add(size)
                .is_some_and(|end| end <= metadata.len()),
            "CLR stream {name} extends past the metadata directory"
        );
        streams.push((name.to_owned(), offset, size));
        cursor = name_end
            .checked_add(1)
            .and_then(|value| value.checked_add(3))
            .map(|value| value & !3)
            .context("CLR stream-header alignment overflow")?;
        ensure!(
            cursor <= metadata.len(),
            "CLR stream headers extend past the metadata directory"
        );
    }
    ensure!(
        names.contains("#Strings")
            && names.contains("#Blob")
            && (names.contains("#~") ^ names.contains("#-")),
        "CLR metadata is missing required tables/string/blob streams"
    );
    let headers_end = cursor;
    let mut extents = streams
        .iter()
        .map(|(name, offset, size)| {
            ensure!(
                *offset >= headers_end,
                "CLR stream {name} overlaps metadata headers"
            );
            Ok((*offset, offset + size, name))
        })
        .collect::<Result<Vec<_>>>()?;
    extents.sort_by_key(|(start, _, _)| *start);
    for pair in extents.windows(2) {
        ensure!(
            pair[0].1 <= pair[1].0,
            "CLR streams {} and {} overlap",
            pair[0].2,
            pair[1].2
        );
    }

    // Canonicalize semantic stream identity rather than hashing unused
    // directory padding. ReadyToRun may rewrite native/layout bytes, but every
    // named metadata stream and its contents must remain byte-identical.
    streams.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical = b"find-my-files:clr-metadata-identity:v1\0".to_vec();
    canonical.extend_from_slice(&(version.len() as u32).to_le_bytes());
    canonical.extend_from_slice(version);
    for (name, offset, size) in streams {
        canonical.extend_from_slice(&(name.len() as u32).to_le_bytes());
        canonical.extend_from_slice(name.as_bytes());
        canonical.extend_from_slice(&(size as u64).to_le_bytes());
        canonical.extend_from_slice(&metadata[offset..offset + size]);
    }
    Ok(checksum::sha256_hex(&canonical))
}

fn sbom_read_u16(bytes: &[u8], offset: usize, label: &str) -> Result<u16> {
    let raw: [u8; 2] = bytes
        .get(offset..offset.saturating_add(2))
        .with_context(|| format!("truncated {label}"))?
        .try_into()
        .with_context(|| format!("invalid {label} width"))?;
    Ok(u16::from_le_bytes(raw))
}

fn sbom_read_u32(bytes: &[u8], offset: usize, label: &str) -> Result<u32> {
    let raw: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .with_context(|| format!("truncated {label}"))?
        .try_into()
        .with_context(|| format!("invalid {label} width"))?;
    Ok(u32::from_le_bytes(raw))
}

#[cfg(windows)]
fn file_version_from_path(path: &Path) -> Result<Option<FileVersion>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut ignored_handle = 0_u32;
    // SAFETY: `wide` is a live NUL-terminated UTF-16 path and the output pointer
    // refers to a live u32 for the synchronous call.
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), &raw mut ignored_handle) };
    if size == 0 {
        return Ok(None);
    }
    let mut buffer = vec![0_u8; size as usize];
    // SAFETY: the output buffer has exactly `size` writable bytes and all
    // pointers remain live for the synchronous call.
    let loaded = unsafe {
        GetFileVersionInfoW(
            wide.as_ptr(),
            0,
            size,
            buffer.as_mut_ptr().cast::<std::ffi::c_void>(),
        )
    };
    ensure!(
        loaded != 0,
        "GetFileVersionInfoW failed for {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );

    let root_query = [b'\\' as u16, 0];
    let mut fixed_ptr = std::ptr::null_mut::<std::ffi::c_void>();
    let mut fixed_len = 0_u32;
    // SAFETY: the version buffer is initialized by GetFileVersionInfoW; query
    // and output pointers stay live during this synchronous call.
    let queried = unsafe {
        VerQueryValueW(
            buffer.as_ptr().cast::<std::ffi::c_void>(),
            root_query.as_ptr(),
            &raw mut fixed_ptr,
            &raw mut fixed_len,
        )
    };
    ensure!(
        queried != 0
            && !fixed_ptr.is_null()
            && fixed_len as usize >= std::mem::size_of::<VS_FIXEDFILEINFO>(),
        "PE has malformed root VS_FIXEDFILEINFO: {}",
        path.display()
    );
    // SAFETY: VerQueryValueW returned at least one complete VS_FIXEDFILEINFO.
    // `read_unaligned` avoids assuming alignment inside the opaque buffer.
    let fixed = unsafe { fixed_ptr.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
    ensure!(
        fixed.dwSignature == 0xfeef_04bd,
        "PE has an invalid VS_FIXEDFILEINFO signature: {}",
        path.display()
    );
    Ok(Some(FileVersion([
        (fixed.dwFileVersionMS >> 16) as u16,
        fixed.dwFileVersionMS as u16,
        (fixed.dwFileVersionLS >> 16) as u16,
        fixed.dwFileVersionLS as u16,
    ])))
}

#[cfg(not(windows))]
fn file_version_from_path(_path: &Path) -> Result<Option<FileVersion>> {
    Ok(None)
}

fn rust_file_evidence(files: &[FileRecord]) -> Result<Vec<FileRecord>> {
    let rust_paths = [
        "FindMyFiles.exe",
        "app/fmf-service.exe",
        "app/fmf_engine.dll",
    ];
    rust_paths
        .iter()
        .map(|path| {
            let record = find_dist_file(files, path)?;
            ensure!(record.is_pe, "Rust shipping input {path} is not a PE");
            Ok(record.clone())
        })
        .collect()
}

fn find_dist_file<'a>(files: &'a [FileRecord], path: &str) -> Result<&'a FileRecord> {
    let mut matches = files
        .iter()
        .filter(|record| record.path.eq_ignore_ascii_case(path));
    let first = matches
        .next()
        .ok_or_else(|| anyhow!("distribution is missing required file {path}"))?;
    ensure!(
        matches.next().is_none(),
        "distribution contains duplicate case-insensitive path {path}"
    );
    Ok(first)
}

fn merge_cargo_boms(
    version: &str,
    raw_boms: &[(&str, &Value)],
    rust_files: &[FileRecord],
) -> Result<Value> {
    ensure!(
        raw_boms.len() == CARGO_RAW_FILES.len(),
        "expected exactly three cargo-sbom documents"
    );
    let expected: BTreeSet<&str> = CARGO_RAW_FILES
        .iter()
        .map(|(package, _)| *package)
        .collect();
    let actual: BTreeSet<&str> = raw_boms.iter().map(|(package, _)| *package).collect();
    ensure!(
        actual == expected && actual.len() == raw_boms.len(),
        "cargo-sbom package set must be exactly {expected:?}, found {actual:?}"
    );

    let mut components = BTreeMap::new();
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut roots = BTreeSet::new();
    let mut root_versions = BTreeSet::new();
    for (package, value) in raw_boms {
        let graph = parse_raw_cargo_bom(package, value)?;
        root_versions.insert(
            required_string(&graph.root_component, "version", "selected cargo root")?.to_owned(),
        );
        ensure!(
            roots.insert(graph.root_ref.clone()),
            "cargo package roots share bom-ref {}",
            graph.root_ref
        );
        for (reference, component) in graph.components {
            if let Some(existing) = components.get(&reference) {
                ensure!(
                    existing == &component,
                    "cargo component {reference} differs between entry-point SBOMs"
                );
            } else {
                components.insert(reference, component);
            }
        }
        for (reference, children) in graph.edges {
            edges.entry(reference).or_default().extend(children);
        }
        ensure!(
            components.contains_key(&graph.root_ref),
            "selected cargo root {} was not retained as a component",
            graph.root_ref
        );
        ensure!(
            graph.root_component == components[&graph.root_ref],
            "selected cargo root {} changed during merge",
            graph.root_ref
        );
    }
    ensure!(
        root_versions.len() == 1,
        "shipped Cargo entry points do not share one workspace version: {root_versions:?}"
    );
    ensure!(
        root_versions.contains(version),
        "shipped Cargo entry-point version set {root_versions:?} does not equal requested release version {version}"
    );

    for component in components.values() {
        let name = required_string(component, "name", "cargo component")?;
        ensure!(
            !DEVELOPER_ONLY_CRATES
                .iter()
                .any(|forbidden| name.eq_ignore_ascii_case(forbidden)),
            "developer-only crate {name} leaked into the shipping Rust SBOM"
        );
    }

    let root_ref = format!("{ENGINE_ROOT_NAME}@{version}");
    ensure!(
        !components.contains_key(&root_ref),
        "synthetic engine root ref collides with a cargo component"
    );
    edges.insert(root_ref.clone(), roots.clone());
    for reference in components.keys() {
        edges.entry(reference.clone()).or_default();
    }

    let properties = file_properties(rust_files, None);
    let root = component(
        "application",
        &root_ref,
        ENGINE_ROOT_NAME,
        version,
        None,
        properties,
    );
    let bom = build_bom(root, components, edges)?;
    validate_final_bom(&bom, ENGINE_ROOT_NAME, version)?;
    Ok(bom)
}

fn parse_raw_cargo_bom(expected_package: &str, bom: &Value) -> Result<RawCargoGraph> {
    let object = required_object(bom, "cargo-sbom document")?;
    let allowed = [
        "bomFormat",
        "components",
        "dependencies",
        "metadata",
        "serialNumber",
        "specVersion",
        "version",
    ];
    reject_unknown_keys(object, &allowed, "cargo-sbom document")?;
    ensure!(
        required_string(bom, "bomFormat", "cargo-sbom document")? == "CycloneDX",
        "cargo-sbom output is not CycloneDX"
    );
    ensure!(
        required_string(bom, "specVersion", "cargo-sbom document")? == "1.6",
        "cargo-sbom output is not CycloneDX 1.6"
    );
    ensure!(
        bom.get("version").and_then(Value::as_u64) == Some(1),
        "cargo-sbom document version must be integer 1"
    );

    let metadata = required_object(
        object
            .get("metadata")
            .ok_or_else(|| anyhow!("cargo-sbom document has no metadata"))?,
        "cargo-sbom metadata",
    )?;
    for (key, value) in metadata {
        if key != "component" {
            reject_reference_fields(value, &format!("metadata.{key}"))?;
        }
    }
    let wrapper = required_object(
        metadata
            .get("component")
            .ok_or_else(|| anyhow!("cargo-sbom metadata has no component"))?,
        "cargo-sbom wrapper component",
    )?;
    ensure!(
        !wrapper.contains_key("bom-ref") && !wrapper.contains_key("version"),
        "cargo-sbom 0.10 wrapper must not masquerade as a package root"
    );
    ensure!(
        required_string_from_object(wrapper, "name", "cargo-sbom wrapper")? == expected_package,
        "cargo-sbom wrapper name does not match selected package {expected_package}"
    );
    ensure!(
        required_string_from_object(wrapper, "type", "cargo-sbom wrapper")? == "application",
        "cargo-sbom wrapper type must be application"
    );
    for (key, value) in wrapper {
        if key != "components" {
            reject_reference_fields(value, &format!("metadata.component.{key}"))?;
        }
    }
    let nested = required_array(
        wrapper
            .get("components")
            .ok_or_else(|| anyhow!("cargo-sbom wrapper has no nested package roots"))?,
        "cargo-sbom package roots",
    )?;
    ensure!(
        nested.len() == 1,
        "cargo-sbom --cargo-package {expected_package} must select exactly one nested package root"
    );
    let root_component = nested[0].clone();
    validate_raw_component(&root_component, "selected cargo package")?;
    ensure!(
        required_string(&root_component, "name", "selected cargo package")? == expected_package,
        "selected cargo package name does not match {expected_package}"
    );
    let root_ref = component_ref(&root_component, "selected cargo package")?.to_owned();

    let mut components = BTreeMap::new();
    components.insert(root_ref.clone(), root_component.clone());
    let raw_components = required_array(
        object
            .get("components")
            .ok_or_else(|| anyhow!("cargo-sbom document has no components"))?,
        "cargo-sbom components",
    )?;
    for value in raw_components {
        validate_raw_component(value, "cargo dependency component")?;
        let reference = component_ref(value, "cargo dependency component")?.to_owned();
        ensure!(
            components
                .insert(reference.clone(), value.clone())
                .is_none(),
            "cargo-sbom contains duplicate bom-ref {reference}"
        );
    }

    let mut edges = BTreeMap::new();
    let dependencies = required_array(
        object
            .get("dependencies")
            .ok_or_else(|| anyhow!("cargo-sbom document has no dependencies"))?,
        "cargo-sbom dependencies",
    )?;
    for dependency in dependencies {
        let dependency_object = required_object(dependency, "cargo dependency edge")?;
        reject_unknown_keys(
            dependency_object,
            &["dependsOn", "ref"],
            "cargo dependency edge",
        )?;
        let reference =
            required_string_from_object(dependency_object, "ref", "cargo dependency edge")?
                .to_owned();
        ensure!(
            components.contains_key(&reference),
            "cargo dependency ref {reference} does not resolve"
        );
        let children_value = dependency_object
            .get("dependsOn")
            .ok_or_else(|| anyhow!("cargo dependency edge {reference} has no dependsOn array"))?;
        let children_array = required_array(children_value, "cargo dependsOn")?;
        let mut children = BTreeSet::new();
        for child in children_array {
            let child_ref = child
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow!("cargo dependsOn entry must be a nonblank string"))?;
            ensure!(
                components.contains_key(child_ref),
                "cargo dependsOn ref {child_ref} does not resolve"
            );
            ensure!(
                children.insert(child_ref.to_owned()),
                "cargo dependency {reference} repeats dependsOn ref {child_ref}"
            );
        }
        ensure!(
            edges.insert(reference.clone(), children).is_none(),
            "cargo-sbom repeats dependency entry {reference}"
        );
    }
    ensure!(
        edges.contains_key(&root_ref),
        "cargo-sbom selected root {root_ref} has no dependency entry"
    );
    for reference in components.keys() {
        edges.entry(reference.clone()).or_default();
    }
    validate_graph(&root_ref, components.keys(), &edges)?;

    Ok(RawCargoGraph {
        root_ref,
        root_component,
        components,
        edges,
    })
}

fn validate_raw_component(component: &Value, label: &str) -> Result<()> {
    let object = required_object(component, label)?;
    let reference = required_string_from_object(object, "bom-ref", label)?;
    ensure!(!reference.trim().is_empty(), "{label} has a blank bom-ref");
    ensure!(
        !object.contains_key("components") && !object.contains_key("services"),
        "{label} contains an unexpected nested component/service graph"
    );
    required_string_from_object(object, "name", label)?;
    required_string_from_object(object, "version", label)?;
    required_string_from_object(object, "type", label)?;
    for (key, value) in object {
        if key != "bom-ref" {
            reject_reference_fields(value, &format!("{label}.{key}"))?;
        }
    }
    Ok(())
}

fn reject_reference_fields(value: &Value, path: &str) -> Result<()> {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                reject_reference_fields(item, &format!("{path}[{index}]"))?;
            }
        }
        Value::Object(object) => {
            for (key, child) in object {
                ensure!(
                    !matches!(key.as_str(), "bom-ref" | "ref" | "dependsOn"),
                    "unsupported reference-bearing field {path}.{key}"
                );
                reject_reference_fields(child, &format!("{path}.{key}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_dotnet_graph(deps: &Value, assets: &Value) -> Result<DotnetGraph> {
    let deps_object = required_object(deps, "FindMyFiles.deps.json")?;
    reject_unknown_keys(
        deps_object,
        &[
            "compilationOptions",
            "libraries",
            "runtimeTarget",
            "runtimes",
            "targets",
        ],
        "FindMyFiles.deps.json",
    )?;
    let runtime_target = required_object(
        deps_object
            .get("runtimeTarget")
            .ok_or_else(|| anyhow!("FindMyFiles.deps.json has no runtimeTarget"))?,
        "deps runtimeTarget",
    )?;
    let target_name = required_string_from_object(runtime_target, "name", "deps runtimeTarget")?;
    ensure!(
        target_name.ends_with("/win-x64"),
        "SBOM supports only the shipped win-x64 target, found {target_name}"
    );
    let targets = required_object(
        deps_object
            .get("targets")
            .ok_or_else(|| anyhow!("FindMyFiles.deps.json has no targets"))?,
        "deps targets",
    )?;
    let target = required_object(
        targets
            .get(target_name)
            .ok_or_else(|| anyhow!("deps runtime target {target_name} does not exist"))?,
        "resolved deps target",
    )?;
    let libraries = required_object(
        deps_object
            .get("libraries")
            .ok_or_else(|| anyhow!("FindMyFiles.deps.json has no libraries"))?,
        "deps libraries",
    )?;

    let mut name_version_to_id = BTreeMap::new();
    let mut root_candidates = Vec::new();
    for id in target.keys() {
        let library = required_object(
            libraries
                .get(id)
                .ok_or_else(|| anyhow!("resolved deps target {id} has no library metadata"))?,
            &format!("deps library {id}"),
        )?;
        let library_type = required_string_from_object(library, "type", "deps library")?;
        if library_type == "project" {
            root_candidates.push(id.clone());
        }
        let (name, version) = split_identity(id)?;
        ensure!(
            name_version_to_id
                .insert((name.to_owned(), version.to_owned()), id.clone())
                .is_none(),
            "deps target repeats package identity {name}/{version}"
        );
    }
    ensure!(
        root_candidates.len() == 1,
        "resolved deps target must contain exactly one project root"
    );
    let root_id = root_candidates.remove(0);
    let (root_name, _) = split_identity(&root_id)?;
    ensure!(
        root_name == APP_ROOT_NAME,
        "resolved project root is {root_name}, expected {APP_ROOT_NAME}"
    );

    let mut nodes = BTreeMap::new();
    let mut root_dependencies = BTreeSet::new();
    for (id, target_value) in target {
        let target_object =
            required_object(target_value, &format!("resolved deps target node {id}"))?;
        reject_unknown_keys(
            target_object,
            &[
                "dependencies",
                "native",
                "resources",
                "runtime",
                "runtimeTargets",
            ],
            &format!("resolved deps target node {id}"),
        )?;
        let library = required_object(
            libraries
                .get(id)
                .ok_or_else(|| anyhow!("deps target {id} has no library metadata"))?,
            &format!("deps library {id}"),
        )?;
        let library_type =
            required_string_from_object(library, "type", &format!("deps library {id}"))?;
        let dependencies = resolve_dotnet_dependencies(
            target_object.get("dependencies"),
            &name_version_to_id,
            id,
        )?;
        if id == &root_id {
            ensure!(
                library_type == "project",
                "deps root {id} must have project library type"
            );
            root_dependencies = dependencies;
            continue;
        }

        let (name, version) = split_identity(id)?;
        let kind = match library_type {
            "package" => DotnetKind::Package,
            "runtimepack" => DotnetKind::RuntimePack,
            "reference" => DotnetKind::Reference,
            other => bail!("unsupported deps library type {other} for {id}"),
        };
        let (component_name, cache_path, content_hash) =
            dotnet_package_metadata(id, name, version, kind, library, assets)?;
        let assets_list = parse_runtime_assets(target_object, id)?;
        nodes.insert(
            id.clone(),
            DotnetNode {
                id: id.clone(),
                version: version.to_owned(),
                component_name,
                kind,
                cache_path,
                content_hash,
                dependencies,
                assets: assets_list,
            },
        );
    }

    for dependency in &root_dependencies {
        ensure!(
            nodes.contains_key(dependency),
            "app root dependency {dependency} does not resolve"
        );
    }
    for node in nodes.values() {
        for dependency in &node.dependencies {
            ensure!(
                nodes.contains_key(dependency),
                "{} dependency {dependency} does not resolve",
                node.id
            );
        }
    }
    Ok(DotnetGraph {
        root_id,
        root_dependencies,
        nodes,
    })
}

fn split_identity(identity: &str) -> Result<(&str, &str)> {
    let (name, version) = identity
        .rsplit_once('/')
        .ok_or_else(|| anyhow!("dependency identity must be Name/Version: {identity}"))?;
    ensure!(
        !name.trim().is_empty() && !version.trim().is_empty(),
        "dependency identity has blank name/version: {identity}"
    );
    Ok((name, version))
}

fn resolve_dotnet_dependencies(
    value: Option<&Value>,
    identities: &BTreeMap<(String, String), String>,
    owner: &str,
) -> Result<BTreeSet<String>> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    let object = required_object(value, &format!("{owner} dependencies"))?;
    let mut resolved = BTreeSet::new();
    for (name, version_value) in object {
        let version = version_value
            .as_str()
            .filter(|version| !version.trim().is_empty())
            .ok_or_else(|| anyhow!("{owner} dependency {name} has no exact version"))?;
        let key = (name.clone(), version.to_owned());
        let id = identities
            .get(&key)
            .ok_or_else(|| anyhow!("{owner} dependency {name}/{version} does not resolve"))?;
        ensure!(
            resolved.insert(id.clone()),
            "{owner} repeats dependency {id}"
        );
    }
    Ok(resolved)
}

fn dotnet_package_metadata(
    id: &str,
    name: &str,
    version: &str,
    kind: DotnetKind,
    deps_library: &Map<String, Value>,
    assets: &Value,
) -> Result<(String, Option<String>, Option<String>)> {
    match kind {
        DotnetKind::Reference => Ok((name.to_owned(), None, None)),
        DotnetKind::Package => {
            let assets_libraries = required_object(
                assets
                    .get("libraries")
                    .ok_or_else(|| anyhow!("project.assets.json has no libraries"))?,
                "project.assets.json libraries",
            )?;
            let (assets_id, assets_library) =
                case_insensitive_object_entry(assets_libraries, id, "assets library")?;
            ensure!(
                assets_id == id,
                "NuGet identity casing drift: deps={id}, assets={assets_id}"
            );
            let assets_library =
                required_object(assets_library, &format!("assets library {assets_id}"))?;
            ensure!(
                required_string_from_object(assets_library, "type", "assets library")? == "package",
                "assets library {id} is not a package"
            );
            let cache_path = required_string_from_object(assets_library, "path", "assets library")?;
            let normalized_cache_path = normalize_archive_path(cache_path)?;
            let expected_cache_path = format!("{}/{}", name.to_ascii_lowercase(), version);
            ensure!(
                normalized_cache_path.eq_ignore_ascii_case(&expected_cache_path),
                "assets cache path {normalized_cache_path} does not match {id}"
            );
            let assets_hash =
                required_string_from_object(assets_library, "sha512", "assets library")?;
            let deps_hash = required_string_from_object(deps_library, "sha512", "deps library")?
                .strip_prefix("sha512-")
                .ok_or_else(|| anyhow!("deps library {id} has an invalid sha512 prefix"))?;
            ensure!(
                deps_hash == assets_hash,
                "NuGet content hash differs between deps and assets for {id}"
            );
            Ok((
                name.to_owned(),
                Some(normalized_cache_path),
                Some(assets_hash.to_owned()),
            ))
        }
        DotnetKind::RuntimePack => {
            let component_name = name
                .strip_prefix("runtimepack.")
                .ok_or_else(|| anyhow!("runtimepack library {id} lacks runtimepack. prefix"))?;
            validate_download_dependency(assets, component_name, version)?;
            Ok((
                component_name.to_owned(),
                Some(format!(
                    "{}/{}",
                    component_name.to_ascii_lowercase(),
                    version
                )),
                None,
            ))
        }
    }
}

fn validate_download_dependency(assets: &Value, name: &str, version: &str) -> Result<()> {
    let project = required_object(
        assets
            .get("project")
            .ok_or_else(|| anyhow!("project.assets.json has no project"))?,
        "assets project",
    )?;
    let frameworks = required_object(
        project
            .get("frameworks")
            .ok_or_else(|| anyhow!("project.assets.json has no project.frameworks"))?,
        "assets frameworks",
    )?;
    let mut matches = 0;
    for framework in frameworks.values() {
        let framework = required_object(framework, "assets framework")?;
        let Some(downloads) = framework.get("downloadDependencies") else {
            continue;
        };
        for download in required_array(downloads, "downloadDependencies")? {
            let download = required_object(download, "download dependency")?;
            let candidate_name =
                required_string_from_object(download, "name", "download dependency")?;
            if !candidate_name.eq_ignore_ascii_case(name) {
                continue;
            }
            let range = required_string_from_object(download, "version", "download dependency")?;
            ensure!(
                is_exact_nuget_range(range, version),
                "runtime pack {name} is not locked to {version}: {range}"
            );
            matches += 1;
        }
    }
    ensure!(
        matches == 1,
        "runtime pack {name}/{version} must have exactly one assets downloadDependency, found {matches}"
    );
    Ok(())
}

fn is_exact_nuget_range(range: &str, version: &str) -> bool {
    let Some(inner) = range
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    else {
        return false;
    };
    let mut bounds = inner.split(',').map(str::trim);
    let first = bounds.next();
    let second = bounds.next();
    first == Some(version) && second == Some(version) && bounds.next().is_none()
}

fn parse_runtime_assets(target: &Map<String, Value>, owner: &str) -> Result<Vec<RuntimeAsset>> {
    let mut assets = Vec::new();
    for (section, transformable) in [("runtime", true), ("native", false)] {
        let Some(value) = target.get(section) else {
            continue;
        };
        let object = required_object(value, &format!("{owner} {section} assets"))?;
        for (source, metadata) in object {
            let source_path = normalize_archive_path(source)?;
            let filename = source_path
                .rsplit('/')
                .next()
                .ok_or_else(|| anyhow!("{owner} has an invalid asset path {source_path}"))?
                .to_owned();
            let metadata = required_object(metadata, &format!("{owner} asset {source_path}"))?;
            let declared_file_version = metadata
                .get("fileVersion")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(|value| parse_file_version(value, &format!("{owner} asset {source_path}")))
                .transpose()?;
            let has_declared_assembly_version = metadata
                .get("assemblyVersion")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty());
            assets.push(RuntimeAsset {
                source_path,
                output_path: format!("app/{filename}"),
                transformable,
                declared_file_version,
                has_declared_assembly_version,
            });
        }
    }
    for section in ["resources", "runtimeTargets"] {
        if target.contains_key(section) {
            bail!(
                "{owner} contains unsupported {section} assets; add a tested output-path mapping before shipping"
            );
        }
    }
    assets.sort_by(|left, right| left.output_path.cmp(&right.output_path));
    let mut outputs = BTreeSet::new();
    for asset in &assets {
        ensure!(
            outputs.insert(asset.output_path.to_ascii_lowercase()),
            "{owner} repeats output asset {}",
            asset.output_path
        );
    }
    Ok(assets)
}

fn case_insensitive_object_entry<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<(&'a str, &'a Value)> {
    let mut matches = object
        .iter()
        .filter(|(candidate, _)| candidate.eq_ignore_ascii_case(key));
    let (actual, value) = matches
        .next()
        .ok_or_else(|| anyhow!("{label} {key} does not exist"))?;
    ensure!(
        matches.next().is_none(),
        "{label} contains case-colliding identity {key}"
    );
    Ok((actual, value))
}

fn load_package_inventories(
    graph: &DotnetGraph,
    assets: &Value,
) -> Result<BTreeMap<String, PackageInventory>> {
    let roots = package_folders(assets)?;
    let mut inventories = BTreeMap::new();
    for node in graph
        .nodes
        .values()
        .filter(|node| node.kind != DotnetKind::Reference)
    {
        let inventory = load_package_inventory(node, &roots)?;
        ensure!(
            inventories.insert(node.id.clone(), inventory).is_none(),
            "duplicate package inventory {}",
            node.id
        );
    }
    Ok(inventories)
}

fn package_folders(assets: &Value) -> Result<Vec<PathBuf>> {
    let folders = required_object(
        assets
            .get("packageFolders")
            .ok_or_else(|| anyhow!("project.assets.json has no packageFolders"))?,
        "assets packageFolders",
    )?;
    ensure!(
        !folders.is_empty(),
        "project.assets.json has no NuGet package cache roots"
    );
    let mut roots = Vec::new();
    for folder in folders.keys() {
        let path = PathBuf::from(folder);
        ensure!(
            path.is_absolute(),
            "NuGet package cache root is not absolute: {folder}"
        );
        let canonical = path
            .canonicalize()
            .with_context(|| format!("canonicalize NuGet package root {}", path.display()))?;
        roots.push(canonical);
    }
    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn load_package_inventory(node: &DotnetNode, roots: &[PathBuf]) -> Result<PackageInventory> {
    let cache_path = node
        .cache_path
        .as_deref()
        .ok_or_else(|| anyhow!("{} has no NuGet cache path", node.id))?;
    let mut candidates = Vec::new();
    for root in roots {
        let candidate = join_safe_relative(root, cache_path)?;
        if candidate.is_dir() {
            let canonical = candidate
                .canonicalize()
                .with_context(|| format!("canonicalize {}", candidate.display()))?;
            ensure!(
                canonical.starts_with(root),
                "NuGet package directory escaped cache root: {}",
                canonical.display()
            );
            candidates.push(canonical);
        }
    }
    ensure!(
        candidates.len() == 1,
        "{} must exist in exactly one NuGet package cache, found {}",
        node.id,
        candidates.len()
    );
    let package_dir = &candidates[0];
    let metadata_path = package_dir.join(".nupkg.metadata");
    let metadata = read_json(&metadata_path)?;
    ensure!(
        required_string(&metadata, "source", ".nupkg.metadata")? == NUGET_SOURCE,
        "{} was restored from an unexpected NuGet source",
        node.id
    );
    let metadata_hash = required_string(&metadata, "contentHash", ".nupkg.metadata")?.to_owned();
    if let Some(expected) = &node.content_hash {
        ensure!(
            &metadata_hash == expected,
            "{} cache contentHash differs from project.assets.json",
            node.id
        );
    }

    let mut archives = Vec::new();
    for entry in fs::read_dir(package_dir)
        .with_context(|| format!("read NuGet package directory {}", package_dir.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("nupkg"))
        {
            archives.push(entry.path());
        }
    }
    ensure!(
        archives.len() == 1,
        "{} must have exactly one restored .nupkg archive, found {}",
        node.id,
        archives.len()
    );
    let archive_file = File::open(&archives[0])
        .with_context(|| format!("open NuGet archive {}", archives[0].display()))?;
    let mut archive = ZipArchive::new(archive_file)
        .with_context(|| format!("parse NuGet archive {}", archives[0].display()))?;
    let mut files = Vec::new();
    let mut folded_paths = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("read {} entry {index}", archives[0].display()))?;
        if entry.is_dir() {
            continue;
        }
        let relative = normalize_archive_path(entry.name())?;
        ensure!(
            folded_paths.insert(relative.to_ascii_lowercase()),
            "{} archive contains case-colliding path {relative}",
            node.id
        );
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("read {} from {}", relative, archives[0].display()))?;
        let is_pe = bytes.starts_with(b"MZ");
        let source_sha256 = checksum::sha256_hex(&bytes);
        let managed_metadata_sha256 = if is_pe {
            managed_metadata_sha256(&bytes)
                .with_context(|| format!("inspect managed metadata in {}/{}", node.id, relative))?
        } else {
            None
        };
        let extracted = join_safe_relative(package_dir, &relative)?;
        let extracted_metadata = match fs::symlink_metadata(&extracted) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect extracted NuGet file {}", extracted.display())
                });
            }
        };
        if let Some(extracted_metadata) = &extracted_metadata {
            ensure!(
                extracted_metadata.is_file() && !fsx::is_reparse_point(extracted_metadata),
                "{} extracted cache entry is a non-file or reparse point: {relative}",
                node.id
            );
            let extracted_bytes = fs::read(&extracted)
                .with_context(|| format!("read extracted NuGet file {}", extracted.display()))?;
            ensure!(
                bytes.len() == extracted_bytes.len()
                    && source_sha256 == checksum::sha256_hex(&extracted_bytes),
                "{} extracted cache file differs from its .nupkg entry: {relative}",
                node.id
            );
        }
        let file_version = if is_pe && extracted_metadata.is_some() {
            file_version_from_path(&extracted)
                .with_context(|| format!("read FileVersion from {}/{}", node.id, relative))?
        } else {
            None
        };
        let source = SourceFile {
            path: relative.clone(),
            sha256: source_sha256,
            size: bytes.len() as u64,
            is_pe,
            managed_metadata_sha256,
            file_version,
        };
        files.push(source);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    ensure!(
        !files.is_empty(),
        "{} NuGet package archive is empty",
        node.id
    );
    Ok(PackageInventory {
        id: node.id.clone(),
        content_hash: metadata_hash,
        files,
    })
}

fn join_safe_relative(root: &Path, relative: &str) -> Result<PathBuf> {
    let normalized = normalize_archive_path(relative)?;
    let mut path = root.to_path_buf();
    for part in normalized.split('/') {
        path.push(part);
    }
    ensure!(
        path.components()
            .all(|component| !matches!(component, Component::ParentDir)),
        "unsafe relative path {relative}"
    );
    Ok(path)
}

fn build_app_bom(
    version: &str,
    graph: &DotnetGraph,
    inventories: &BTreeMap<String, PackageInventory>,
    dist_files: &[FileRecord],
) -> Result<Value> {
    let (root_name, resolved_version) = split_identity(&graph.root_id)?;
    ensure!(
        root_name == APP_ROOT_NAME && resolved_version == version,
        "resolved app root {} does not equal requested product {APP_ROOT_NAME}/{version}",
        graph.root_id
    );
    let expected_inventory_ids: BTreeSet<&str> = graph
        .nodes
        .values()
        .filter(|node| node.kind != DotnetKind::Reference)
        .map(|node| node.id.as_str())
        .collect();
    let actual_inventory_ids: BTreeSet<&str> = inventories.keys().map(String::as_str).collect();
    ensure!(
        actual_inventory_ids == expected_inventory_ids,
        "NuGet inventory set differs from the resolved runtime graph"
    );
    for (id, inventory) in inventories {
        ensure!(
            inventory.id == *id,
            "NuGet inventory key/identity differs for {id}"
        );
        if let Some(expected) = &graph.nodes[id].content_hash {
            ensure!(
                &inventory.content_hash == expected,
                "NuGet inventory contentHash differs for {id}"
            );
        }
    }

    let mut assignments: BTreeMap<String, Assignment> = BTreeMap::new();
    assign_first_party(dist_files, &mut assignments)?;

    let mut source_by_hash: BTreeMap<(u64, String), Vec<(String, SourceFile)>> = BTreeMap::new();
    for (id, inventory) in inventories {
        for source in &inventory.files {
            if !is_runtime_source_path(&source.path) {
                continue;
            }
            source_by_hash
                .entry((source.size, source.sha256.clone()))
                .or_default()
                .push((id.clone(), source.clone()));
        }
    }
    for matches in source_by_hash.values_mut() {
        matches.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.path.cmp(&right.1.path))
        });
    }

    for file in dist_files {
        let key = file.path.to_ascii_lowercase();
        if assignments.contains_key(&key) {
            continue;
        }
        let Some(matches) = source_by_hash.get(&(file.size, file.sha256.clone())) else {
            continue;
        };
        let owners: BTreeSet<&str> = matches.iter().map(|(id, _)| id.as_str()).collect();
        ensure!(
            owners.len() == 1,
            "shipped file {} has an ambiguous exact hash match in packages {owners:?}",
            file.path
        );
        let (id, source) = &matches[0];
        assignments.insert(
            key,
            Assignment {
                owner: OwnerKind::Package,
                component_id: Some(id.clone()),
                source: Some(source.clone()),
                transformed: false,
            },
        );
    }

    reconcile_declared_assets(graph, inventories, dist_files, &mut assignments)?;

    for file in dist_files {
        let key = file.path.to_ascii_lowercase();
        if assignments.contains_key(&key) {
            continue;
        }
        ensure!(
            !file.is_pe,
            "shipped PE {} is not owned by the app, Rust, or a resolved NuGet/runtime pack",
            file.path
        );
        assignments.insert(
            key,
            Assignment {
                owner: OwnerKind::App,
                component_id: None,
                source: None,
                transformed: false,
            },
        );
    }
    ensure!(
        assignments.len() == dist_files.len(),
        "not every shipped file received exactly one owner"
    );

    let mut component_files: BTreeMap<String, Vec<(&FileRecord, &Assignment)>> = BTreeMap::new();
    let mut app_files = Vec::new();
    for file in dist_files {
        let assignment = &assignments[&file.path.to_ascii_lowercase()];
        match assignment.owner {
            OwnerKind::App => app_files.push(file.clone()),
            OwnerKind::Rust => {}
            OwnerKind::Package => {
                let id = assignment
                    .component_id
                    .as_ref()
                    .ok_or_else(|| anyhow!("package assignment has no component id"))?;
                component_files
                    .entry(id.clone())
                    .or_default()
                    .push((file, assignment));
            }
        }
    }

    let included: BTreeSet<String> = component_files.keys().cloned().collect();
    let mut components = BTreeMap::new();
    let mut id_to_ref = BTreeMap::new();
    for id in &included {
        let node = &graph.nodes[id];
        ensure!(
            node.kind != DotnetKind::Reference,
            "reference-only dependency {id} cannot own shipped bytes"
        );
        let reference = nuget_ref(&node.component_name, &node.version);
        ensure!(
            id_to_ref.insert(id.clone(), reference.clone()).is_none(),
            "duplicate .NET component identity {id}"
        );
        let mut properties = vec![property(
            "find-my-files:nuget-content-hash-sha512-base64",
            &inventories[id].content_hash,
        )];
        let mut evidence = component_files.remove(id).unwrap_or_default();
        evidence.sort_by(|left, right| left.0.path.cmp(&right.0.path));
        for (file, assignment) in evidence {
            properties.push(property(
                "find-my-files:shipped-file-sha256",
                &format!("{}={}", file.path, file.sha256),
            ));
            let source = assignment.source.as_ref().ok_or_else(|| {
                anyhow!("package-owned file {} has no source evidence", file.path)
            })?;
            properties.push(property(
                "find-my-files:source-file-sha256",
                &format!("{}={}", source.path, source.sha256),
            ));
            if assignment.transformed {
                let metadata_sha256 = file.managed_metadata_sha256.as_deref().ok_or_else(|| {
                    anyhow!(
                        "transformed file {} lost its validated CLR metadata identity",
                        file.path
                    )
                })?;
                let file_version = file.file_version.ok_or_else(|| {
                    anyhow!(
                        "transformed file {} lost its validated FileVersion",
                        file.path
                    )
                })?;
                properties.push(property(
                    "find-my-files:publish-transform",
                    &format!("{}=ready-to-run", file.path),
                ));
                properties.push(property(
                    "find-my-files:ready-to-run-managed-metadata-sha256",
                    &format!("{}={metadata_sha256}", file.path),
                ));
                properties.push(property(
                    "find-my-files:ready-to-run-file-version",
                    &format!("{}={file_version}", file.path),
                ));
            }
        }
        sort_properties(&mut properties);
        let kind = if node.kind == DotnetKind::RuntimePack {
            "framework"
        } else {
            "library"
        };
        let component = component(
            kind,
            &reference,
            &node.component_name,
            &node.version,
            Some(&reference),
            properties,
        );
        ensure!(
            components.insert(reference.clone(), component).is_none(),
            "duplicate CycloneDX ref {reference}"
        );
    }

    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let root_ref = format!("{APP_ROOT_NAME}@{version}");
    let root_children = graph
        .root_dependencies
        .iter()
        .filter_map(|id| id_to_ref.get(id).cloned())
        .collect();
    edges.insert(root_ref.clone(), root_children);
    for id in &included {
        let reference = id_to_ref[id].clone();
        let children = graph.nodes[id]
            .dependencies
            .iter()
            .filter_map(|dependency| id_to_ref.get(dependency).cloned())
            .collect();
        edges.insert(reference, children);
    }

    let mut root_properties = file_properties(&app_files, None);
    root_properties.push(property("find-my-files:resolved-deps-root", &graph.root_id));
    sort_properties(&mut root_properties);
    let root = component(
        "application",
        &root_ref,
        APP_ROOT_NAME,
        version,
        None,
        root_properties,
    );
    let bom = build_bom(root, components, edges)?;
    validate_final_bom(&bom, APP_ROOT_NAME, version)?;
    Ok(bom)
}

fn assign_first_party(
    dist_files: &[FileRecord],
    assignments: &mut BTreeMap<String, Assignment>,
) -> Result<()> {
    for (path, _) in publish::FIRST_PARTY_PES {
        let record = find_dist_file(dist_files, path)?;
        ensure!(record.is_pe, "first-party signing input {path} is not a PE");
        ensure!(
            record.authenticode_payload_sha256.is_some(),
            "first-party signing input {path} lacks the shared Authenticode payload identity"
        );
        let owner = match *path {
            "app/FindMyFiles.exe" | "app/FindMyFiles.dll" => OwnerKind::App,
            "FindMyFiles.exe" | "app/fmf-service.exe" | "app/fmf_engine.dll" => OwnerKind::Rust,
            _ => bail!(
                "new first-party PE {path} has no explicit SBOM ownership; classify it deliberately"
            ),
        };
        ensure!(
            assignments
                .insert(
                    path.to_ascii_lowercase(),
                    Assignment {
                        owner,
                        component_id: None,
                        source: None,
                        transformed: false,
                    },
                )
                .is_none(),
            "first-party PE list contains duplicate path {path}"
        );
    }
    Ok(())
}

fn reconcile_declared_assets(
    graph: &DotnetGraph,
    inventories: &BTreeMap<String, PackageInventory>,
    dist_files: &[FileRecord],
    assignments: &mut BTreeMap<String, Assignment>,
) -> Result<()> {
    let pruned: BTreeSet<String> = prune::shipped_prune_set()
        .map(str::to_ascii_lowercase)
        .collect();
    for file in dist_files {
        let filename = file
            .path
            .rsplit('/')
            .next()
            .ok_or_else(|| anyhow!("invalid distribution path {}", file.path))?;
        ensure!(
            !pruned.contains(&filename.to_ascii_lowercase()),
            "pruned dependency artifact unexpectedly survives in dist: {}",
            file.path
        );
    }

    let mut declared_outputs: BTreeMap<String, String> = BTreeMap::new();
    for node in graph.nodes.values() {
        for asset in &node.assets {
            let output_key = asset.output_path.to_ascii_lowercase();
            if let Some(previous) = declared_outputs.insert(output_key.clone(), node.id.clone()) {
                ensure!(
                    previous == node.id,
                    "resolved dependencies {previous} and {} both claim {}",
                    node.id,
                    asset.output_path
                );
            }
            let dist = dist_files
                .iter()
                .find(|file| file.path.eq_ignore_ascii_case(&asset.output_path));
            let Some(dist) = dist else {
                let filename = asset
                    .output_path
                    .rsplit('/')
                    .next()
                    .ok_or_else(|| anyhow!("invalid output path {}", asset.output_path))?;
                ensure!(
                    pruned.contains(&filename.to_ascii_lowercase()),
                    "{} declares runtime asset {} but it is absent from the final dist",
                    node.id,
                    asset.output_path
                );
                continue;
            };
            if node.kind == DotnetKind::Reference {
                bail!(
                    "reference-only dependency {} unexpectedly emitted {}",
                    node.id,
                    asset.output_path
                );
            }
            if let Some(existing) = assignments.get(&output_key) {
                ensure!(
                    existing.owner == OwnerKind::Package
                        && existing.component_id.as_deref() == Some(node.id.as_str()),
                    "declared asset {} is attributed to a different owner",
                    asset.output_path
                );
                continue;
            }
            ensure!(
                asset.transformable && asset.has_declared_assembly_version,
                "{} differs from its NuGet source but is not a declared managed runtime asset",
                asset.output_path
            );
            let declared_file_version = asset.declared_file_version.ok_or_else(|| {
                anyhow!(
                    "{} differs from its NuGet source but has no declared FileVersion",
                    asset.output_path
                )
            })?;
            ensure!(
                dist.is_pe,
                "transformed runtime asset {} is not a PE",
                asset.output_path
            );
            let source = find_declared_source(&inventories[&node.id], asset)?;
            ensure!(
                source.is_pe,
                "NuGet source for transformed asset {} is not a PE",
                asset.output_path
            );
            ensure!(
                source.sha256 != dist.sha256,
                "exact source hash for {} should have been reconciled before transform handling",
                asset.output_path
            );
            let source_metadata = source.managed_metadata_sha256.as_deref().ok_or_else(|| {
                anyhow!(
                    "NuGet source for transformed asset {} has no valid CLR metadata",
                    asset.output_path
                )
            })?;
            let shipped_metadata = dist.managed_metadata_sha256.as_deref().ok_or_else(|| {
                anyhow!(
                    "shipped transformed asset {} has no valid CLR metadata",
                    asset.output_path
                )
            })?;
            ensure!(
                source_metadata == shipped_metadata,
                "ReadyToRun transform changed CLR metadata identity for {}",
                asset.output_path
            );
            let source_file_version = source.file_version.ok_or_else(|| {
                anyhow!(
                    "NuGet source for transformed asset {} has no VS_FIXEDFILEINFO FileVersion",
                    asset.output_path
                )
            })?;
            let shipped_file_version = dist.file_version.ok_or_else(|| {
                anyhow!(
                    "shipped transformed asset {} has no VS_FIXEDFILEINFO FileVersion",
                    asset.output_path
                )
            })?;
            ensure!(
                source_file_version == declared_file_version
                    && shipped_file_version == declared_file_version,
                "ReadyToRun FileVersion mismatch for {}: declared {}, source {}, shipped {}",
                asset.output_path,
                declared_file_version,
                source_file_version,
                shipped_file_version
            );
            assignments.insert(
                output_key,
                Assignment {
                    owner: OwnerKind::Package,
                    component_id: Some(node.id.clone()),
                    source: Some(source),
                    transformed: true,
                },
            );
        }
    }
    Ok(())
}

fn find_declared_source(inventory: &PackageInventory, asset: &RuntimeAsset) -> Result<SourceFile> {
    let direct: Vec<&SourceFile> = inventory
        .files
        .iter()
        .filter(|source| source.path.eq_ignore_ascii_case(&asset.source_path))
        .collect();
    if direct.len() == 1 {
        ensure!(
            is_runtime_source_path(&direct[0].path),
            "{} declared asset resolves to a build/analyzer-only archive path {}",
            inventory.id,
            direct[0].path
        );
        return Ok(direct[0].clone());
    }
    ensure!(
        direct.is_empty(),
        "{} package contains case-colliding source path {}",
        inventory.id,
        asset.source_path
    );
    let filename = asset
        .source_path
        .rsplit('/')
        .next()
        .ok_or_else(|| anyhow!("invalid declared source path {}", asset.source_path))?;
    let by_name: Vec<&SourceFile> = inventory
        .files
        .iter()
        .filter(|source| {
            source
                .path
                .rsplit('/')
                .next()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(filename))
        })
        .collect();
    ensure!(
        by_name.len() == 1,
        "{} asset {} must resolve to exactly one archive entry by basename, found {}",
        inventory.id,
        asset.source_path,
        by_name.len()
    );
    ensure!(
        is_runtime_source_path(&by_name[0].path),
        "{} declared asset resolves to a build/analyzer-only archive path {}",
        inventory.id,
        by_name[0].path
    );
    Ok(by_name[0].clone())
}

fn is_runtime_source_path(path: &str) -> bool {
    ["lib/", "metadata/", "runtimes/", "runtimes-framework/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

fn file_properties(files: &[FileRecord], prefix: Option<&str>) -> Vec<Value> {
    let mut sorted = files.to_vec();
    sorted.sort_by(|left, right| left.path.cmp(&right.path));
    let mut properties = sorted
        .iter()
        .map(|file| {
            let path = prefix.map_or_else(
                || file.path.clone(),
                |value| format!("{value}/{}", file.path),
            );
            if let Some(payload_sha256) = &file.authenticode_payload_sha256 {
                property(
                    "find-my-files:first-party-pe-authenticode-payload-sha256",
                    &format!("{path}={payload_sha256}"),
                )
            } else {
                property(
                    "find-my-files:shipped-file-sha256",
                    &format!("{path}={}", file.sha256),
                )
            }
        })
        .collect::<Vec<_>>();
    sort_properties(&mut properties);
    properties
}

fn property(name: &str, value: &str) -> Value {
    json!({"name": name, "value": value})
}

fn sort_properties(properties: &mut [Value]) {
    properties.sort_by(|left, right| {
        let left_name = left.get("name").and_then(Value::as_str).unwrap_or_default();
        let right_name = right
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let left_value = left
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let right_value = right
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default();
        left_name
            .cmp(right_name)
            .then_with(|| left_value.cmp(right_value))
    });
}

fn nuget_ref(name: &str, version: &str) -> String {
    format!("pkg:nuget/{name}@{version}")
}

fn component(
    kind: &str,
    reference: &str,
    name: &str,
    version: &str,
    purl: Option<&str>,
    properties: Vec<Value>,
) -> Value {
    let mut object = Map::new();
    object.insert("type".to_owned(), Value::String(kind.to_owned()));
    object.insert("bom-ref".to_owned(), Value::String(reference.to_owned()));
    object.insert("name".to_owned(), Value::String(name.to_owned()));
    object.insert("version".to_owned(), Value::String(version.to_owned()));
    if let Some(purl) = purl {
        object.insert("purl".to_owned(), Value::String(purl.to_owned()));
    }
    if !properties.is_empty() {
        object.insert("properties".to_owned(), Value::Array(properties));
    }
    Value::Object(object)
}

fn build_bom(
    root: Value,
    components: BTreeMap<String, Value>,
    mut edges: BTreeMap<String, BTreeSet<String>>,
) -> Result<Value> {
    let root_ref = component_ref(&root, "SBOM root")?.to_owned();
    ensure!(
        !components.contains_key(&root_ref),
        "SBOM root ref collides with a component: {root_ref}"
    );
    for reference in components.keys() {
        edges.entry(reference.clone()).or_default();
    }
    ensure!(
        edges.contains_key(&root_ref),
        "SBOM root {root_ref} has no dependency entry"
    );
    validate_graph(&root_ref, components.keys(), &edges)?;

    let component_values = components.into_values().collect::<Vec<_>>();
    let dependencies = edges
        .into_iter()
        .map(|(reference, children)| {
            json!({
                "ref": reference,
                "dependsOn": children.into_iter().collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "$schema": CYCLONEDX_SCHEMA,
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {
            "component": root
        },
        "components": component_values,
        "dependencies": dependencies
    }))
}

fn validate_final_bom(bom: &Value, expected_name: &str, expected_version: &str) -> Result<()> {
    let object = required_object(bom, "final CycloneDX BOM")?;
    let expected_keys: BTreeSet<&str> = [
        "$schema",
        "bomFormat",
        "components",
        "dependencies",
        "metadata",
        "specVersion",
        "version",
    ]
    .into_iter()
    .collect();
    let actual_keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    ensure!(
        actual_keys == expected_keys,
        "final CycloneDX document has unsupported/missing sections: {actual_keys:?}"
    );
    ensure!(
        required_string(bom, "$schema", "final BOM")? == CYCLONEDX_SCHEMA,
        "final BOM has an unexpected CycloneDX schema"
    );
    ensure!(
        required_string(bom, "bomFormat", "final BOM")? == "CycloneDX"
            && required_string(bom, "specVersion", "final BOM")? == "1.6"
            && bom.get("version").and_then(Value::as_u64) == Some(1),
        "final BOM identity is not CycloneDX 1.6/version 1"
    );
    let metadata = required_object(
        object
            .get("metadata")
            .ok_or_else(|| anyhow!("final BOM has no metadata"))?,
        "final BOM metadata",
    )?;
    reject_unknown_keys(metadata, &["component"], "final BOM metadata")?;
    let root = metadata
        .get("component")
        .ok_or_else(|| anyhow!("final BOM metadata has no component"))?;
    validate_final_component(root, "final BOM root")?;
    ensure!(
        required_string(root, "name", "final BOM root")? == expected_name,
        "final BOM root name does not match {expected_name}"
    );
    ensure!(
        required_string(root, "version", "final BOM root")? == expected_version,
        "final BOM root version does not match {expected_version}"
    );
    ensure!(
        required_string(root, "type", "final BOM root")? == "application",
        "final BOM root type must be application"
    );
    let root_ref = component_ref(root, "final BOM root")?.to_owned();

    let mut refs = BTreeSet::new();
    let components = required_array(
        object
            .get("components")
            .ok_or_else(|| anyhow!("final BOM has no components"))?,
        "final BOM components",
    )?;
    ensure!(
        !components.is_empty(),
        "final BOM must contain at least one component"
    );
    for component in components {
        validate_final_component(component, "final BOM component")?;
        let reference = component_ref(component, "final BOM component")?;
        ensure!(
            reference != root_ref,
            "final BOM root is duplicated in components"
        );
        ensure!(
            refs.insert(reference.to_owned()),
            "final BOM repeats component ref {reference}"
        );
    }

    let mut edges = BTreeMap::new();
    let dependencies = required_array(
        object
            .get("dependencies")
            .ok_or_else(|| anyhow!("final BOM has no dependencies"))?,
        "final BOM dependencies",
    )?;
    for dependency in dependencies {
        let dependency = required_object(dependency, "final dependency")?;
        reject_unknown_keys(dependency, &["dependsOn", "ref"], "final dependency")?;
        let reference =
            required_string_from_object(dependency, "ref", "final dependency")?.to_owned();
        ensure!(
            reference == root_ref || refs.contains(&reference),
            "final dependency ref {reference} does not resolve"
        );
        let mut children = BTreeSet::new();
        for child in required_array(
            dependency
                .get("dependsOn")
                .ok_or_else(|| anyhow!("final dependency {reference} has no dependsOn"))?,
            "final dependsOn",
        )? {
            let child = child
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| anyhow!("final dependsOn must be a nonblank string"))?;
            ensure!(
                refs.contains(child),
                "final dependsOn ref {child} does not resolve to a component"
            );
            ensure!(
                children.insert(child.to_owned()),
                "final dependency {reference} repeats {child}"
            );
        }
        ensure!(
            edges.insert(reference.clone(), children).is_none(),
            "final BOM repeats dependency entry {reference}"
        );
    }
    ensure!(
        edges.len() == refs.len() + 1
            && edges.contains_key(&root_ref)
            && refs.iter().all(|reference| edges.contains_key(reference)),
        "final BOM must have exactly one dependency entry for root and every component"
    );
    validate_graph(&root_ref, refs.iter(), &edges)
}

fn validate_final_component(value: &Value, label: &str) -> Result<()> {
    let object = required_object(value, label)?;
    for required in ["bom-ref", "name", "type", "version"] {
        required_string_from_object(object, required, label)?;
    }
    ensure!(
        !object.contains_key("components") && !object.contains_key("services"),
        "{label} contains a nested graph"
    );
    if let Some(properties) = object.get("properties") {
        let properties = required_array(properties, &format!("{label} properties"))?;
        for property in properties {
            let property = required_object(property, &format!("{label} property"))?;
            reject_unknown_keys(property, &["name", "value"], &format!("{label} property"))?;
            required_string_from_object(property, "name", &format!("{label} property"))?;
            required_string_from_object(property, "value", &format!("{label} property"))?;
        }
    }
    Ok(())
}

fn validate_graph<'a>(
    root_ref: &str,
    component_refs: impl Iterator<Item = &'a String>,
    edges: &BTreeMap<String, BTreeSet<String>>,
) -> Result<()> {
    let known: BTreeSet<String> = std::iter::once(root_ref.to_owned())
        .chain(component_refs.cloned())
        .collect();
    ensure!(
        edges.keys().all(|reference| known.contains(reference)),
        "dependency graph contains an unknown source ref"
    );
    for (reference, children) in edges {
        for child in children {
            ensure!(
                known.contains(child),
                "dependency {reference} points to unknown ref {child}"
            );
        }
    }
    let mut reachable = BTreeSet::new();
    let mut queue = VecDeque::from([root_ref.to_owned()]);
    while let Some(reference) = queue.pop_front() {
        if !reachable.insert(reference.clone()) {
            continue;
        }
        if let Some(children) = edges.get(&reference) {
            queue.extend(children.iter().cloned());
        }
    }
    let unreachable: Vec<&String> = known.difference(&reachable).collect();
    ensure!(
        unreachable.is_empty(),
        "dependency graph contains components outside the root closure: {unreachable:?}"
    );
    Ok(())
}

fn component_ref<'a>(value: &'a Value, label: &str) -> Result<&'a str> {
    required_string(value, "bom-ref", label)
}

fn required_string<'a>(value: &'a Value, key: &str, label: &str) -> Result<&'a str> {
    let object = required_object(value, label)?;
    required_string_from_object(object, key, label)
}

fn required_string_from_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow!("{label} has no nonblank string {key}"))
}

fn required_object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| anyhow!("{label} must be an object"))
}

fn required_array<'a>(value: &'a Value, label: &str) -> Result<&'a Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| anyhow!("{label} must be an array"))
}

fn reject_unknown_keys(object: &Map<String, Value>, allowed: &[&str], label: &str) -> Result<()> {
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    ensure!(
        unknown.is_empty(),
        "{label} contains unsupported fields: {unknown:?}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_json_rejects_duplicate_keys_at_every_depth() {
        assert!(parse_json_strict(r#"{"root":1,"root":2}"#).is_err());
        assert!(parse_json_strict(r#"{"outer":{"value":1,"value":2}}"#).is_err());
        assert!(parse_json_strict(r#"[{"value":1,"value":2}]"#).is_err());
        assert_eq!(
            parse_json_strict(r#"{"array":[null,true,-1,2,3.5],"object":{"key":"value"}}"#)
                .unwrap(),
            json!({
                "array": [null, true, -1, 2, 3.5],
                "object": {"key": "value"}
            })
        );
    }
    use std::sync::atomic::{AtomicU64, Ordering};

    static SCRATCH_ID: AtomicU64 = AtomicU64::new(0);

    fn scratch(tag: &str) -> PathBuf {
        let id = SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("xtask-sbom-{tag}-{}-{id}", std::process::id()))
    }

    fn cargo_component(name: &str, version: &str, reference: &str) -> Value {
        json!({
            "type": "library",
            "bom-ref": reference,
            "name": name,
            "version": version,
            "licenses": [{"license": {"expression": "MIT"}}]
        })
    }

    fn raw_cargo_bom(
        name: &str,
        root_ref: &str,
        components: Vec<Value>,
        edges: Vec<(&str, Vec<&str>)>,
    ) -> Value {
        json!({
            "bomFormat": "CycloneDX",
            "specVersion": "1.6",
            "serialNumber": "urn:uuid:non-deterministic-generator-value",
            "version": 1,
            "metadata": {
                "timestamp": "2026-01-01T00:00:00.000Z",
                "authors": [{"name": "CI User"}],
                "tools": [{"name": "cargo-sbom", "version": "0.10.0"}],
                "component": {
                    "type": "application",
                    "name": name,
                    "components": [{
                        "type": "application",
                        "bom-ref": root_ref,
                        "name": name,
                        "version": "0.1.1"
                    }]
                }
            },
            "components": components,
            "dependencies": edges.into_iter().map(|(reference, children)| {
                json!({"ref": reference, "dependsOn": children})
            }).collect::<Vec<_>>()
        })
    }

    fn complete_raw_set() -> Vec<(&'static str, Value)> {
        vec![
            (
                "fmf-service",
                raw_cargo_bom(
                    "fmf-service",
                    "cargo:fmf-service@0.1.1",
                    vec![cargo_component("shared", "1.0.0", "cargo:shared@1.0.0")],
                    vec![
                        ("cargo:fmf-service@0.1.1", vec!["cargo:shared@1.0.0"]),
                        ("cargo:shared@1.0.0", vec![]),
                    ],
                ),
            ),
            (
                "fmf-ffi",
                raw_cargo_bom(
                    "fmf-ffi",
                    "cargo:fmf-ffi@0.1.1",
                    vec![cargo_component("shared", "1.0.0", "cargo:shared@1.0.0")],
                    vec![
                        ("cargo:fmf-ffi@0.1.1", vec!["cargo:shared@1.0.0"]),
                        ("cargo:shared@1.0.0", vec![]),
                    ],
                ),
            ),
            (
                "fmf-launcher",
                raw_cargo_bom(
                    "fmf-launcher",
                    "cargo:fmf-launcher@0.1.1",
                    vec![cargo_component(
                        "fmf-buildstamp",
                        "0.1.1",
                        "cargo:fmf-buildstamp@0.1.1",
                    )],
                    vec![
                        (
                            "cargo:fmf-launcher@0.1.1",
                            vec!["cargo:fmf-buildstamp@0.1.1"],
                        ),
                        ("cargo:fmf-buildstamp@0.1.1", vec![]),
                    ],
                ),
            ),
        ]
    }

    fn test_file(path: &str, bytes: &[u8]) -> FileRecord {
        let is_pe = bytes.starts_with(b"MZ");
        let is_first_party = publish::FIRST_PARTY_PES
            .iter()
            .any(|(candidate, _)| *candidate == path);
        FileRecord {
            path: path.to_owned(),
            sha256: checksum::sha256_hex(bytes),
            size: bytes.len() as u64,
            is_pe,
            authenticode_payload_sha256: is_first_party
                .then(|| checksum::sha256_hex(format!("payload:{path}").as_bytes())),
            managed_metadata_sha256: is_pe
                .then(|| checksum::sha256_hex(b"fixture-managed-metadata")),
            file_version: is_pe.then_some(FileVersion([1, 0, 0, 0])),
        }
    }

    fn rust_evidence() -> Vec<FileRecord> {
        vec![
            test_file("FindMyFiles.exe", b"MZlauncher"),
            test_file("app/fmf-service.exe", b"MZservice"),
            test_file("app/fmf_engine.dll", b"MZffi"),
        ]
    }

    #[test]
    fn cargo_merge_preserves_three_roots_and_is_deterministic() {
        let raw = complete_raw_set();
        let borrowed = raw
            .iter()
            .map(|(name, value)| (*name, value))
            .collect::<Vec<_>>();
        let first = merge_cargo_boms("0.1.1", &borrowed, &rust_evidence()).unwrap();
        let second = merge_cargo_boms("0.1.1", &borrowed, &rust_evidence()).unwrap();
        assert_eq!(first, second);

        let component_names: BTreeSet<&str> = first["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| component["name"].as_str().unwrap())
            .collect();
        for root in ["fmf-service", "fmf-ffi", "fmf-launcher"] {
            assert!(
                component_names.contains(root),
                "missing package root {root}"
            );
        }
        let root_edge = first["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|edge| edge["ref"] == "fmf-engine@0.1.1")
            .unwrap();
        assert_eq!(root_edge["dependsOn"].as_array().unwrap().len(), 3);
        assert!(first.get("serialNumber").is_none());
        assert!(first["metadata"].get("timestamp").is_none());
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(encoded.contains("first-party-pe-authenticode-payload-sha256"));
        for file in rust_evidence() {
            assert!(
                !encoded.contains(&format!(
                    "find-my-files:shipped-file-sha256\",\"value\":\"{}={}",
                    file.path, file.sha256
                )),
                "first-party PE full unsigned hash must not become SBOM identity"
            );
        }
    }

    #[test]
    fn cargo_wrapper_must_remain_an_unversioned_single_root_wrapper() {
        let mut raw = complete_raw_set().remove(0).1;
        raw["metadata"]["component"]["bom-ref"] = json!("wrong");
        assert!(parse_raw_cargo_bom("fmf-service", &raw).is_err());

        let mut raw = complete_raw_set().remove(0).1;
        let second = raw["metadata"]["component"]["components"][0].clone();
        raw["metadata"]["component"]["components"]
            .as_array_mut()
            .unwrap()
            .push(second);
        assert!(parse_raw_cargo_bom("fmf-service", &raw).is_err());
    }

    #[test]
    fn cargo_merge_rejects_collision_dangling_orphan_and_developer_only_crates() {
        let mut set = complete_raw_set();
        set[1].1["components"][0]["version"] = json!("2.0.0");
        let borrowed = set
            .iter()
            .map(|(name, value)| (*name, value))
            .collect::<Vec<_>>();
        assert!(merge_cargo_boms("0.1.1", &borrowed, &rust_evidence()).is_err());

        let mut set = complete_raw_set();
        for (_, raw) in &mut set {
            raw["metadata"]["component"]["components"][0]["version"] = json!("0.1.2");
        }
        let borrowed = set
            .iter()
            .map(|(name, value)| (*name, value))
            .collect::<Vec<_>>();
        assert!(
            merge_cargo_boms("0.1.1", &borrowed, &rust_evidence()).is_err(),
            "mutually agreeing Cargo roots must still equal the requested release version"
        );

        let mut raw = complete_raw_set().remove(0).1;
        raw["dependencies"][0]["dependsOn"][0] = json!("missing");
        assert!(parse_raw_cargo_bom("fmf-service", &raw).is_err());

        let mut raw = complete_raw_set().remove(0).1;
        raw["components"]
            .as_array_mut()
            .unwrap()
            .push(cargo_component("orphan", "1.0.0", "cargo:orphan@1.0.0"));
        raw["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(json!({"ref": "cargo:orphan@1.0.0", "dependsOn": []}));
        assert!(parse_raw_cargo_bom("fmf-service", &raw).is_err());

        let mut set = complete_raw_set();
        set[0].1["components"][0] = cargo_component("criterion", "1.0.0", "cargo:shared@1.0.0");
        let borrowed = set
            .iter()
            .map(|(name, value)| (*name, value))
            .collect::<Vec<_>>();
        assert!(merge_cargo_boms("0.1.1", &borrowed, &rust_evidence()).is_err());
    }

    #[test]
    fn cargo_raw_rejects_unknown_reference_bearing_sections() {
        let mut raw = complete_raw_set().remove(0).1;
        raw["services"] = json!([{"bom-ref": "hidden"}]);
        assert!(parse_raw_cargo_bom("fmf-service", &raw).is_err());

        let mut raw = complete_raw_set().remove(0).1;
        raw["components"][0]["evidence"] = json!({"identity": {"ref": "hidden"}});
        assert!(parse_raw_cargo_bom("fmf-service", &raw).is_err());
    }

    fn app_graph_fixture() -> DotnetGraph {
        let nodes = BTreeMap::from([
            (
                "Package.A/1.0.0".to_owned(),
                DotnetNode {
                    id: "Package.A/1.0.0".to_owned(),
                    version: "1.0.0".to_owned(),
                    component_name: "Package.A".to_owned(),
                    kind: DotnetKind::Package,
                    cache_path: Some("package.a/1.0.0".to_owned()),
                    content_hash: Some("package-a-content-hash".to_owned()),
                    dependencies: BTreeSet::new(),
                    assets: vec![RuntimeAsset {
                        source_path: "lib/net10.0/Package.A.dll".to_owned(),
                        output_path: "app/Package.A.dll".to_owned(),
                        transformable: true,
                        declared_file_version: Some(FileVersion([1, 0, 0, 0])),
                        has_declared_assembly_version: true,
                    }],
                },
            ),
            (
                "Microsoft.Web.WebView2/1.0.0".to_owned(),
                DotnetNode {
                    id: "Microsoft.Web.WebView2/1.0.0".to_owned(),
                    version: "1.0.0".to_owned(),
                    component_name: "Microsoft.Web.WebView2".to_owned(),
                    kind: DotnetKind::Package,
                    cache_path: Some("microsoft.web.webview2/1.0.0".to_owned()),
                    content_hash: Some("webview-content-hash".to_owned()),
                    dependencies: BTreeSet::new(),
                    assets: vec![RuntimeAsset {
                        source_path: "runtimes/win-x64/native/WebView2Loader.dll".to_owned(),
                        output_path: "app/WebView2Loader.dll".to_owned(),
                        transformable: false,
                        declared_file_version: Some(FileVersion([1, 0, 0, 0])),
                        has_declared_assembly_version: false,
                    }],
                },
            ),
        ]);
        DotnetGraph {
            root_id: "FindMyFiles/0.1.1".to_owned(),
            root_dependencies: BTreeSet::from([
                "Microsoft.Web.WebView2/1.0.0".to_owned(),
                "Package.A/1.0.0".to_owned(),
            ]),
            nodes,
        }
    }

    fn package_inventory(id: &str, hash: &str, path: &str, bytes: &[u8]) -> PackageInventory {
        PackageInventory {
            id: id.to_owned(),
            content_hash: hash.to_owned(),
            files: vec![SourceFile {
                path: path.to_owned(),
                sha256: checksum::sha256_hex(bytes),
                size: bytes.len() as u64,
                is_pe: bytes.starts_with(b"MZ"),
                managed_metadata_sha256: bytes
                    .starts_with(b"MZ")
                    .then(|| checksum::sha256_hex(b"fixture-managed-metadata")),
                file_version: bytes
                    .starts_with(b"MZ")
                    .then_some(FileVersion([1, 0, 0, 0])),
            }],
        }
    }

    fn app_dist_fixture(package_bytes: &[u8]) -> Vec<FileRecord> {
        vec![
            test_file("FindMyFiles.exe", b"MZlauncher"),
            test_file("app/FindMyFiles.exe", b"MZapphost"),
            test_file("app/FindMyFiles.dll", b"MZapp"),
            test_file("app/fmf-service.exe", b"MZservice"),
            test_file("app/fmf_engine.dll", b"MZffi"),
            test_file("app/Package.A.dll", package_bytes),
            test_file("README.txt", b"readme"),
        ]
    }

    fn app_inventories(package_bytes: &[u8]) -> BTreeMap<String, PackageInventory> {
        BTreeMap::from([
            (
                "Package.A/1.0.0".to_owned(),
                package_inventory(
                    "Package.A/1.0.0",
                    "package-a-content-hash",
                    "lib/net10.0/Package.A.dll",
                    package_bytes,
                ),
            ),
            (
                "Microsoft.Web.WebView2/1.0.0".to_owned(),
                package_inventory(
                    "Microsoft.Web.WebView2/1.0.0",
                    "webview-content-hash",
                    "runtimes/win-x64/native/WebView2Loader.dll",
                    b"MZwebview",
                ),
            ),
        ])
    }

    #[test]
    fn app_bom_contains_only_packages_with_shipped_bytes_and_excludes_pruned_deps() {
        let graph = app_graph_fixture();
        let dist = app_dist_fixture(b"MZpackage-a");
        let inventories = app_inventories(b"MZpackage-a");
        let bom = build_app_bom("0.1.1", &graph, &inventories, &dist).unwrap();
        let names: BTreeSet<&str> = bom["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|component| component["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, BTreeSet::from(["Package.A"]));
        assert!(!serde_json::to_string(&bom).unwrap().contains("WebView2"));
    }

    #[test]
    fn app_bom_records_a_strict_ready_to_run_transform() {
        let graph = app_graph_fixture();
        let dist = app_dist_fixture(b"MZready-to-run-output");
        let inventories = app_inventories(b"MZpackage-source");
        let bom = build_app_bom("0.1.1", &graph, &inventories, &dist).unwrap();
        let encoded = serde_json::to_string(&bom).unwrap();
        assert!(encoded.contains("ready-to-run"));
        assert!(encoded.contains("ready-to-run-managed-metadata-sha256"));
        assert!(encoded.contains("ready-to-run-file-version"));
        assert!(encoded.contains(&checksum::sha256_hex(b"MZpackage-source")));
        assert!(encoded.contains(&checksum::sha256_hex(b"MZready-to-run-output")));

        let mut drifted_dist = dist.clone();
        drifted_dist
            .iter_mut()
            .find(|file| file.path == "app/Package.A.dll")
            .unwrap()
            .managed_metadata_sha256 = Some(checksum::sha256_hex(b"different-metadata"));
        assert!(
            build_app_bom("0.1.1", &graph, &inventories, &drifted_dist).is_err(),
            "semantic CLR metadata drift must not be labeled ReadyToRun"
        );

        let mut drifted_version = dist;
        drifted_version
            .iter_mut()
            .find(|file| file.path == "app/Package.A.dll")
            .unwrap()
            .file_version = Some(FileVersion([1, 0, 0, 1]));
        assert!(
            build_app_bom("0.1.1", &graph, &inventories, &drifted_version).is_err(),
            "shipped FileVersion must equal source and deps declarations"
        );
    }

    #[test]
    fn app_bom_rejects_unowned_pe_or_ambiguous_package_hash() {
        let graph = app_graph_fixture();
        let mut dist = app_dist_fixture(b"MZpackage-a");
        dist.push(test_file("app/Unknown.dll", b"MZunknown"));
        assert!(build_app_bom("0.1.1", &graph, &app_inventories(b"MZpackage-a"), &dist).is_err());

        let mut inventories = app_inventories(b"MZpackage-a");
        inventories
            .get_mut("Microsoft.Web.WebView2/1.0.0")
            .unwrap()
            .files[0] = SourceFile {
            path: "lib/duplicate.dll".to_owned(),
            sha256: checksum::sha256_hex(b"MZpackage-a"),
            size: b"MZpackage-a".len() as u64,
            is_pe: true,
            managed_metadata_sha256: Some(checksum::sha256_hex(b"fixture-managed-metadata")),
            file_version: Some(FileVersion([1, 0, 0, 0])),
        };
        let dist = app_dist_fixture(b"MZpackage-a");
        assert!(build_app_bom("0.1.1", &graph, &inventories, &dist).is_err());

        let mut inventories = app_inventories(b"MZpackage-a");
        inventories.get_mut("Package.A/1.0.0").unwrap().files[0].path =
            "tools/Package.A.dll".to_owned();
        assert!(build_app_bom(
            "0.1.1",
            &graph,
            &inventories,
            &app_dist_fixture(b"MZpackage-a")
        )
        .is_err());
    }

    #[test]
    fn app_bom_rejects_native_hash_drift_and_is_deterministic() {
        let mut graph = app_graph_fixture();
        graph.nodes.get_mut("Package.A/1.0.0").unwrap().assets[0].transformable = false;
        let dist = app_dist_fixture(b"MZchanged-native");
        assert!(build_app_bom(
            "0.1.1",
            &graph,
            &app_inventories(b"MZpackage-source"),
            &dist
        )
        .is_err());

        let graph = app_graph_fixture();
        let dist = app_dist_fixture(b"MZpackage-a");
        let inventories = app_inventories(b"MZpackage-a");
        let first = build_app_bom("0.1.1", &graph, &inventories, &dist).unwrap();
        let second = build_app_bom("0.1.1", &graph, &inventories, &dist).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn exact_nuget_ranges_are_fail_closed() {
        assert!(is_exact_nuget_range("[10.0.10, 10.0.10]", "10.0.10"));
        assert!(!is_exact_nuget_range("[10.0.10, 11.0.0]", "10.0.10"));
        assert!(!is_exact_nuget_range("10.0.10", "10.0.10"));
    }

    #[test]
    fn archive_paths_reject_traversal_absolute_and_empty_segments() {
        for bad in ["../evil", "/absolute", "C:/drive", "a//b", "a/./b"] {
            assert!(normalize_archive_path(bad).is_err(), "{bad}");
        }
        assert_eq!(
            normalize_archive_path("runtimes\\win-x64\\native\\x.dll").unwrap(),
            "runtimes/win-x64/native/x.dll"
        );
    }

    fn synthetic_managed_pe(metadata_marker: u8, native_marker: u8) -> Vec<u8> {
        const PE: usize = 0x80;
        const COFF: usize = PE + 4;
        const OPTIONAL: usize = COFF + 20;
        const OPTIONAL_SIZE: usize = 0xf0;
        const SECTION: usize = OPTIONAL + OPTIONAL_SIZE;
        const RAW: usize = 0x200;
        const CLR: usize = RAW;
        const METADATA: usize = RAW + 0x100;
        const METADATA_SIZE: usize = 0x100;
        let mut bytes = vec![native_marker; 0x600];
        bytes[..RAW].fill(0);
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&(PE as u32).to_le_bytes());
        bytes[PE..PE + 4].copy_from_slice(b"PE\0\0");
        bytes[COFF..COFF + 2].copy_from_slice(&0x8664_u16.to_le_bytes());
        bytes[COFF + 2..COFF + 4].copy_from_slice(&1_u16.to_le_bytes());
        bytes[COFF + 16..COFF + 18].copy_from_slice(&(OPTIONAL_SIZE as u16).to_le_bytes());
        bytes[OPTIONAL..OPTIONAL + 2].copy_from_slice(&0x020b_u16.to_le_bytes());
        bytes[OPTIONAL + 60..OPTIONAL + 64].copy_from_slice(&0x200_u32.to_le_bytes());
        bytes[OPTIONAL + 108..OPTIONAL + 112].copy_from_slice(&16_u32.to_le_bytes());
        let clr_directory = OPTIONAL + 112 + CLR_DIRECTORY_INDEX * 8;
        bytes[clr_directory..clr_directory + 4].copy_from_slice(&0x1000_u32.to_le_bytes());
        bytes[clr_directory + 4..clr_directory + 8].copy_from_slice(&0x48_u32.to_le_bytes());
        bytes[SECTION + 8..SECTION + 12].copy_from_slice(&0x400_u32.to_le_bytes());
        bytes[SECTION + 12..SECTION + 16].copy_from_slice(&0x1000_u32.to_le_bytes());
        bytes[SECTION + 16..SECTION + 20].copy_from_slice(&0x400_u32.to_le_bytes());
        bytes[SECTION + 20..SECTION + 24].copy_from_slice(&(RAW as u32).to_le_bytes());

        bytes[CLR..CLR + 4].copy_from_slice(&0x48_u32.to_le_bytes());
        bytes[CLR + 8..CLR + 12].copy_from_slice(&0x1100_u32.to_le_bytes());
        bytes[CLR + 12..CLR + 16].copy_from_slice(&(METADATA_SIZE as u32).to_le_bytes());
        bytes[METADATA..METADATA + 4].copy_from_slice(&0x424a_5342_u32.to_le_bytes());
        bytes[METADATA + 4..METADATA + 6].copy_from_slice(&1_u16.to_le_bytes());
        bytes[METADATA + 6..METADATA + 8].copy_from_slice(&1_u16.to_le_bytes());
        bytes[METADATA + 12..METADATA + 16].copy_from_slice(&12_u32.to_le_bytes());
        bytes[METADATA + 16..METADATA + 28].copy_from_slice(b"v4.0.30319\0\0");
        bytes[METADATA + 30..METADATA + 32].copy_from_slice(&3_u16.to_le_bytes());

        let mut stream = METADATA + 32;
        for (offset, size, name) in [
            (0x80_u32, 0x10_u32, b"#~\0".as_slice()),
            (0x90_u32, 0x10_u32, b"#Strings\0".as_slice()),
            (0xa0_u32, 0x10_u32, b"#Blob\0".as_slice()),
        ] {
            bytes[stream..stream + 4].copy_from_slice(&offset.to_le_bytes());
            bytes[stream + 4..stream + 8].copy_from_slice(&size.to_le_bytes());
            bytes[stream + 8..stream + 8 + name.len()].copy_from_slice(name);
            stream = (stream + 8 + name.len() + 3) & !3;
        }
        bytes[METADATA + 0x80..METADATA + 0xb0].fill(metadata_marker);
        bytes
    }

    #[test]
    fn managed_metadata_identity_is_stable_across_native_transform_only() {
        let source = synthetic_managed_pe(0x51, 0x11);
        let transformed = synthetic_managed_pe(0x51, 0x77);
        let drifted = synthetic_managed_pe(0x52, 0x77);
        let source_identity = managed_metadata_sha256(&source).unwrap();
        assert_eq!(
            source_identity,
            managed_metadata_sha256(&transformed).unwrap(),
            "native ReadyToRun bytes may change without changing CLR metadata identity"
        );
        assert_ne!(source_identity, managed_metadata_sha256(&drifted).unwrap());
        assert!(managed_metadata_sha256(b"MZtruncated").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn published_managed_identity_and_file_version_are_readable_when_present() {
        let root = paths::dist_dir();
        if !root.is_dir() {
            return;
        }
        let files = collect_dist_files(&root).unwrap();
        let app = find_dist_file(&files, "app/FindMyFiles.dll").unwrap();
        assert!(
            app.managed_metadata_sha256.is_some(),
            "published managed entry assembly must expose validated CLR metadata"
        );
        assert!(
            app.file_version.is_some(),
            "published managed entry assembly must expose VS_FIXEDFILEINFO"
        );
        assert!(
            app.authenticode_payload_sha256.is_some(),
            "published first-party PE must use the shared unsigned payload identity"
        );
    }

    fn empty_final_bom(name: &str, version: &str) -> Value {
        let reference = format!("{name}@{version}");
        let evidence_ref = format!("{name}-evidence@{version}");
        let root = component("application", &reference, name, version, None, Vec::new());
        let evidence = component(
            "library",
            &evidence_ref,
            &format!("{name}-evidence"),
            version,
            None,
            Vec::new(),
        );
        build_bom(
            root,
            BTreeMap::from([(evidence_ref.clone(), evidence)]),
            BTreeMap::from([
                (reference, BTreeSet::from([evidence_ref.clone()])),
                (evidence_ref, BTreeSet::new()),
            ]),
        )
        .unwrap()
    }

    #[test]
    fn final_bom_pair_is_exact_versioned_and_exclusive() {
        let base = scratch("final-pair");
        fs::create_dir_all(&base).unwrap();
        write_json_atomic(
            &base.join("fmf-engine.cdx.json"),
            &empty_final_bom(ENGINE_ROOT_NAME, "0.1.1"),
        )
        .unwrap();
        write_json_atomic(
            &base.join("app.cdx.json"),
            &empty_final_bom(APP_ROOT_NAME, "0.1.1"),
        )
        .unwrap();
        verify_final_pair_at(&base, "0.1.1").unwrap();

        fs::write(base.join("unexpected.txt"), b"x").unwrap();
        assert!(verify_final_pair_at(&base, "0.1.1").is_err());
        fs::remove_file(base.join("unexpected.txt")).unwrap();

        write_json_atomic(
            &base.join("app.cdx.json"),
            &empty_final_bom(APP_ROOT_NAME, "0.1.2"),
        )
        .unwrap();
        assert!(verify_final_pair_at(&base, "0.1.1").is_err());
        fsx::force_remove_dir_all(&base).unwrap();
    }
}
