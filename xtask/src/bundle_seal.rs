//! Deterministic, fail-closed manifests for the assembled release bundle.
//!
//! The manifest lives next to (never inside) `build/dist/FindMyFiles`, so it can
//! prove the exact bundle tree without changing the tree it describes.  The
//! unsigned manifest also pins an Authenticode-stable payload identity for every
//! entry in [`crate::publish::FIRST_PARTY_PES`].  A signed transition is accepted
//! only when those exact files gained one structurally valid terminal
//! `WIN_CERTIFICATE`, their normalized payloads stayed identical, and every
//! other shipped byte stayed identical.

use crate::{checksum, fsx, paths, publish::FIRST_PARTY_PES, version};
use anyhow::{bail, ensure, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 2;
const SHA256_HEX_LEN: usize = 64;
const WIN_CERT_REVISION_2_0: u16 = 0x0200;
const WIN_CERT_TYPE_PKCS_SIGNED_DATA: u16 = 0x0002;
const IMAGE_DIRECTORY_ENTRY_SECURITY: usize = 4;

/// The exact Authenticode state asserted by a bundle manifest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum BundleState {
    Unsigned,
    Signed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleManifest {
    schema_version: u32,
    source_commit: String,
    state: BundleState,
    files: Vec<BundleFileIdentity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BundleFileIdentity {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pe_payload_sha256: Option<String>,
    #[serde(skip)]
    pub is_pe: bool,
}

#[derive(Debug)]
struct PeLayout {
    checksum_offset: usize,
    security_directory_offset: usize,
    certificate: Option<CertificateTable>,
}

#[derive(Debug)]
struct CertificateTable {
    offset: usize,
}

/// Create the canonical manifest for the current distribution tree.
pub fn run_seal(state: BundleState) -> Result<()> {
    let dist = paths::dist_dir();
    let manifest_path = manifest_path(state);
    if state == BundleState::Signed {
        // A caller cannot accidentally bless an arbitrary already-signed tree:
        // creating the signed manifest itself requires the signing-only proof.
        verify_signed_transition_at(&dist, &paths::unsigned_bundle_manifest())?;
    }
    seal_to(&dist, &manifest_path, state)?;
    println!(
        "bundle-seal: wrote exact {state:?} manifest {}",
        manifest_path.display()
    );
    Ok(())
}

/// Verify the current distribution tree byte-for-byte against its canonical
/// manifest, including the declared Authenticode state of all first-party PEs.
pub fn run_verify(state: BundleState) -> Result<()> {
    let dist = paths::dist_dir();
    let manifest_path = manifest_path(state);
    verify_at(&dist, &manifest_path, state)?;
    println!(
        "bundle-verify: exact {state:?} bundle matches {}",
        manifest_path.display()
    );
    Ok(())
}

/// Prove that the current signed bundle is the only allowed transition from the
/// canonical unsigned manifest.
pub fn run_verify_signed_transition() -> Result<()> {
    let dist = paths::dist_dir();
    let unsigned_manifest = paths::unsigned_bundle_manifest();
    verify_signed_transition_at(&dist, &unsigned_manifest)?;
    println!(
        "bundle-verify-signed-transition: signing-only transition matches {}",
        unsigned_manifest.display()
    );
    Ok(())
}

fn manifest_path(state: BundleState) -> PathBuf {
    match state {
        BundleState::Unsigned => paths::unsigned_bundle_manifest(),
        BundleState::Signed => paths::signed_bundle_manifest(),
    }
}

fn seal_to(root: &Path, destination: &Path, state: BundleState) -> Result<()> {
    ensure_manifest_outside_bundle(root, destination)?;
    let manifest = BundleManifest {
        schema_version: SCHEMA_VERSION,
        source_commit: read_bundle_source_commit(root)?,
        state,
        files: collect_bundle_files(root, state)?,
    };
    validate_manifest(&manifest)?;
    let mut encoded = serde_json::to_vec_pretty(&manifest).context("serialize bundle manifest")?;
    encoded.push(b'\n');
    fsx::write_file_atomic(destination, &encoded)
        .with_context(|| format!("atomically write {}", destination.display()))
}

fn verify_at(root: &Path, manifest_path: &Path, expected_state: BundleState) -> Result<()> {
    ensure_manifest_outside_bundle(root, manifest_path)?;
    let expected = read_manifest(manifest_path)?;
    ensure!(
        expected.state == expected_state,
        "bundle manifest state is {:?}, expected {expected_state:?}",
        expected.state
    );
    let actual_source_commit = read_bundle_source_commit(root)?;
    ensure!(
        expected.source_commit == actual_source_commit,
        "bundle source commit differs: manifest {}, BUILDINFO {}",
        expected.source_commit,
        actual_source_commit
    );
    let actual = collect_bundle_files(root, expected_state)?;
    compare_exact(&expected.files, &actual)
}

fn verify_signed_transition_at(root: &Path, unsigned_manifest_path: &Path) -> Result<()> {
    ensure_manifest_outside_bundle(root, unsigned_manifest_path)?;
    let unsigned = read_manifest(unsigned_manifest_path)?;
    ensure!(
        unsigned.state == BundleState::Unsigned,
        "signed transition requires an unsigned manifest, got {:?}",
        unsigned.state
    );
    let signed_source_commit = read_bundle_source_commit(root)?;
    ensure!(
        unsigned.source_commit == signed_source_commit,
        "signed bundle source commit differs: unsigned manifest {}, BUILDINFO {}",
        unsigned.source_commit,
        signed_source_commit
    );
    let signed = collect_bundle_files(root, BundleState::Signed)?;
    ensure!(
        unsigned.files.len() == signed.len(),
        "signed bundle file count changed: unsigned {}, signed {}",
        unsigned.files.len(),
        signed.len()
    );

    let targets = signing_targets()?;
    for (before, after) in unsigned.files.iter().zip(&signed) {
        ensure!(
            before.path == after.path,
            "signed bundle file set changed at '{}' versus '{}'",
            before.path,
            after.path
        );
        if targets.contains(&before.path) {
            ensure!(
                before.sha256 != after.sha256,
                "first-party signing target '{}' did not change",
                before.path
            );
            ensure!(
                before.pe_payload_sha256 == after.pe_payload_sha256,
                "signing changed normalized PE payload '{}': unsigned {:?}, signed {:?}",
                before.path,
                before.pe_payload_sha256,
                after.pe_payload_sha256
            );
        } else {
            ensure!(
                before.size == after.size && before.sha256 == after.sha256,
                "non-signing bundle file '{}' changed",
                before.path
            );
            ensure!(
                before.pe_payload_sha256.is_none() && after.pe_payload_sha256.is_none(),
                "non-signing file '{}' unexpectedly carries a PE payload identity",
                before.path
            );
        }
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<BundleManifest> {
    let bytes =
        fs::read(path).with_context(|| format!("read bundle manifest {}", path.display()))?;
    let manifest: BundleManifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse bundle manifest {}", path.display()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn read_bundle_source_commit(root: &Path) -> Result<String> {
    let path = root.join("BUILDINFO.txt");
    let source = fs::read_to_string(&path)
        .with_context(|| format!("read bundle source identity from {}", path.display()))?;
    version::parse_buildinfo_source_commit(&source)
        .with_context(|| format!("parse bundle source identity from {}", path.display()))
}

fn validate_manifest(manifest: &BundleManifest) -> Result<()> {
    ensure!(
        manifest.schema_version == SCHEMA_VERSION,
        "unsupported bundle manifest schema {}, expected {SCHEMA_VERSION}",
        manifest.schema_version
    );
    version::validate_source_commit(&manifest.source_commit)
        .context("bundle manifest has an invalid source_commit")?;
    ensure!(
        !manifest.files.is_empty(),
        "bundle manifest must describe at least one file"
    );
    let targets = signing_targets()?;
    let mut previous: Option<&str> = None;
    let mut folded = BTreeMap::<String, &str>::new();
    let mut observed_targets = BTreeSet::new();
    for file in &manifest.files {
        validate_relative_path(&file.path)?;
        validate_sha256(&file.sha256, &format!("full hash for '{}'", file.path))?;
        if let Some(payload) = &file.pe_payload_sha256 {
            validate_sha256(payload, &format!("PE payload hash for '{}'", file.path))?;
        }
        if let Some(prior) = previous {
            ensure!(
                prior < file.path.as_str(),
                "bundle manifest paths are not strictly sorted: '{prior}' then '{}'",
                file.path
            );
        }
        previous = Some(&file.path);

        let folded_path = fold_path(&file.path);
        if let Some(prior) = folded.insert(folded_path, &file.path) {
            bail!(
                "bundle manifest contains case-fold-colliding paths '{prior}' and '{}'",
                file.path
            );
        }

        if targets.contains(&file.path) {
            ensure!(
                file.pe_payload_sha256.is_some(),
                "first-party PE '{}' has no normalized payload hash",
                file.path
            );
            observed_targets.insert(file.path.clone());
        } else {
            ensure!(
                file.pe_payload_sha256.is_none(),
                "non-signing file '{}' must not carry a PE payload hash",
                file.path
            );
        }
    }
    ensure!(
        observed_targets == targets,
        "bundle manifest first-party PE set differs from FIRST_PARTY_PES: expected {targets:?}, got {observed_targets:?}"
    );
    Ok(())
}

fn compare_exact(expected: &[BundleFileIdentity], actual: &[BundleFileIdentity]) -> Result<()> {
    ensure!(
        expected.len() == actual.len(),
        "bundle file count differs: manifest {}, tree {}",
        expected.len(),
        actual.len()
    );
    for (wanted, got) in expected.iter().zip(actual) {
        ensure!(
            wanted.path == got.path,
            "bundle file set differs at manifest '{}' versus tree '{}'",
            wanted.path,
            got.path
        );
        ensure!(
            wanted.size == got.size,
            "bundle file '{}' size changed: manifest {}, tree {}",
            wanted.path,
            wanted.size,
            got.size
        );
        ensure!(
            wanted.sha256 == got.sha256,
            "bundle file '{}' SHA-256 changed: manifest {}, tree {}",
            wanted.path,
            wanted.sha256,
            got.sha256
        );
        ensure!(
            wanted.pe_payload_sha256 == got.pe_payload_sha256,
            "bundle file '{}' normalized PE payload changed",
            wanted.path
        );
    }
    Ok(())
}

/// Enumerate and validate an exact bundle tree for release consumers such as
/// sealing and SBOM generation. This is the single implementation of path,
/// case-collision, reparse-point, file-set, and first-party PE-state policy.
pub fn collect_bundle_files(root: &Path, state: BundleState) -> Result<Vec<BundleFileIdentity>> {
    let root_metadata =
        fs::symlink_metadata(root).with_context(|| format!("inspect bundle {}", root.display()))?;
    ensure!(
        root_metadata.is_dir() && !fsx::is_reparse_point(&root_metadata),
        "bundle root must be a real directory, not a symlink/reparse point: {}",
        root.display()
    );

    let targets = signing_targets()?;
    let mut files = Vec::new();
    let mut seen_entries = BTreeMap::<String, String>::new();
    walk_bundle(root, root, &targets, state, &mut seen_entries, &mut files)?;
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    ensure!(
        !files.is_empty(),
        "bundle tree is empty: {}",
        root.display()
    );

    let observed: BTreeSet<_> = files
        .iter()
        .filter(|file| file.pe_payload_sha256.is_some())
        .map(|file| file.path.clone())
        .collect();
    ensure!(
        observed == targets,
        "bundle first-party PE set differs from FIRST_PARTY_PES: expected {targets:?}, got {observed:?}"
    );
    Ok(files)
}

fn walk_bundle(
    root: &Path,
    directory: &Path,
    targets: &BTreeSet<String>,
    state: BundleState,
    seen_entries: &mut BTreeMap<String, String>,
    files: &mut Vec<BundleFileIdentity>,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("read bundle directory {}", directory.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("enumerate bundle directory {}", directory.display()))?;
    entries.sort_unstable_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let relative = relative_path(root, &path)?;
        let folded = fold_path(&relative);
        if let Some(prior) = seen_entries.insert(folded, relative.clone()) {
            bail!("bundle contains case-fold-colliding entries '{prior}' and '{relative}'");
        }

        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("inspect bundle entry {}", path.display()))?;
        ensure!(
            !fsx::is_reparse_point(&metadata),
            "bundle entry is a symlink/reparse point: {relative}"
        );
        if metadata.is_dir() {
            walk_bundle(root, &path, targets, state, seen_entries, files)?;
            continue;
        }
        ensure!(
            metadata.is_file(),
            "bundle entry is neither a regular file nor directory: {relative}"
        );

        let bytes =
            fs::read(&path).with_context(|| format!("read bundle file {}", path.display()))?;
        let after = fs::symlink_metadata(&path)
            .with_context(|| format!("reinspect bundle file {}", path.display()))?;
        ensure!(
            after.is_file()
                && !fsx::is_reparse_point(&after)
                && metadata.len() == after.len()
                && after.len() == bytes.len() as u64,
            "bundle file changed type or size while being sealed: {relative}"
        );

        let pe_payload_sha256 = if targets.contains(&relative) {
            Some(
                normalized_pe_payload_sha256(&bytes, state).with_context(|| {
                    format!("validate first-party PE '{relative}' as {state:?}")
                })?,
            )
        } else {
            None
        };
        files.push(BundleFileIdentity {
            path: relative,
            size: bytes.len() as u64,
            sha256: checksum::sha256_hex(&bytes),
            pe_payload_sha256,
            is_pe: bytes.starts_with(b"MZ"),
        });
    }
    Ok(())
}

fn signing_targets() -> Result<BTreeSet<String>> {
    ensure!(
        !FIRST_PARTY_PES.is_empty(),
        "FIRST_PARTY_PES must not be empty"
    );
    let mut targets = BTreeSet::new();
    let mut stage_names = BTreeSet::new();
    for (path, stage_name) in FIRST_PARTY_PES {
        validate_relative_path(path)?;
        ensure!(
            !stage_name.is_empty()
                && !stage_name.contains('/')
                && !stage_name.contains('\\')
                && stage_names.insert((*stage_name).to_owned()),
            "FIRST_PARTY_PES contains an empty, nested, or duplicate stage name '{stage_name}'"
        );
        ensure!(
            targets.insert((*path).to_owned()),
            "FIRST_PARTY_PES contains duplicate path '{path}'"
        );
    }
    Ok(targets)
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("bundle entry {} escaped {}", path.display(), root.display()))?;
    let mut components = Vec::new();
    for component in relative.components() {
        let value = component
            .as_os_str()
            .to_str()
            .with_context(|| format!("bundle path is not Unicode: {}", path.display()))?;
        validate_path_component(value)?;
        components.push(value);
    }
    ensure!(
        !components.is_empty(),
        "bundle entry unexpectedly resolved to its root"
    );
    let normalized = components.join("/");
    validate_relative_path(&normalized)?;
    Ok(normalized)
}

fn validate_relative_path(path: &str) -> Result<()> {
    ensure!(!path.is_empty(), "bundle path must not be empty");
    ensure!(
        !path.starts_with('/') && !path.ends_with('/') && !path.contains('\\'),
        "bundle path is not normalized with safe forward slashes: '{path}'"
    );
    for component in path.split('/') {
        validate_path_component(component)?;
    }
    Ok(())
}

fn validate_path_component(component: &str) -> Result<()> {
    ensure!(
        !component.is_empty() && component != "." && component != "..",
        "bundle path contains an empty or traversal component '{component}'"
    );
    ensure!(
        !component.ends_with('.') && !component.ends_with(' '),
        "bundle path component has a Windows-ambiguous suffix: '{component}'"
    );
    ensure!(
        !component
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character)),
        "bundle path component contains a Windows-unsafe character: '{component}'"
    );

    let device_stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_uppercase();
    let reserved = matches!(device_stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || device_stem
            .strip_prefix("COM")
            .is_some_and(is_reserved_device_number)
        || device_stem
            .strip_prefix("LPT")
            .is_some_and(is_reserved_device_number);
    ensure!(
        !reserved,
        "bundle path component is a reserved Windows device name: '{component}'"
    );
    ensure!(
        component.is_ascii(),
        "bundle path component must use ASCII only: '{component}'"
    );
    Ok(())
}

fn is_reserved_device_number(suffix: &str) -> bool {
    matches!(
        suffix,
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
    )
}

fn fold_path(path: &str) -> String {
    // Paths are validated ASCII before folding, so this exactly models the
    // distributable namespace we permit without Unicode case ambiguities.
    path.to_ascii_lowercase()
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    ensure!(
        value.len() == SHA256_HEX_LEN
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{label} is not a lowercase SHA-256"
    );
    Ok(())
}

fn ensure_manifest_outside_bundle(root: &Path, manifest_path: &Path) -> Result<()> {
    ensure!(
        manifest_path != root && !manifest_path.starts_with(root),
        "bundle manifest must live outside the bundle it describes: {}",
        manifest_path.display()
    );
    Ok(())
}

/// Return the shared Authenticode-stable SHA-256 identity used by bundle
/// sealing and shipped-file SBOM attribution.
pub fn normalized_pe_payload_sha256(bytes: &[u8], state: BundleState) -> Result<String> {
    let layout = parse_pe(bytes)?;
    match (state, &layout.certificate) {
        (BundleState::Unsigned, None) | (BundleState::Signed, Some(_)) => {}
        (BundleState::Unsigned, Some(_)) => {
            bail!("unsigned first-party PE already has a certificate table")
        }
        (BundleState::Signed, None) => bail!("signed first-party PE has no certificate table"),
    }

    let payload_end = layout
        .certificate
        .as_ref()
        .map_or(bytes.len(), |table| table.offset);
    ensure!(
        layout.checksum_offset + 4 <= layout.security_directory_offset
            && layout.security_directory_offset + 8 <= payload_end,
        "PE normalization fields are out of order or overlap the certificate table"
    );

    let mut hasher = Sha256::new();
    hasher.update(&bytes[..layout.checksum_offset]);
    hasher.update([0_u8; 4]);
    hasher.update(&bytes[layout.checksum_offset + 4..layout.security_directory_offset]);
    hasher.update([0_u8; 8]);
    hasher.update(&bytes[layout.security_directory_offset + 8..payload_end]);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(SHA256_HEX_LEN);
    for byte in digest {
        let _ = write!(encoded, "{byte:02x}");
    }
    Ok(encoded)
}

fn parse_pe(bytes: &[u8]) -> Result<PeLayout> {
    ensure!(bytes.len() >= 0x40, "PE is shorter than its DOS header");
    ensure!(&bytes[..2] == b"MZ", "PE has no MZ signature");
    let pe_offset = read_u32(bytes, 0x3c, "DOS e_lfanew")? as usize;
    ensure!(
        pe_offset >= 0x40,
        "PE header overlaps or precedes the DOS header"
    );
    ensure!(
        bytes.get(pe_offset..pe_offset.saturating_add(4)) == Some(b"PE\0\0".as_slice()),
        "PE signature is missing or truncated"
    );

    let coff = pe_offset
        .checked_add(4)
        .context("PE COFF header offset overflow")?;
    let optional = coff
        .checked_add(20)
        .context("PE optional header offset overflow")?;
    let section_count = usize::from(read_u16(bytes, coff + 2, "COFF NumberOfSections")?);
    ensure!(section_count != 0, "PE contains no sections");
    let optional_size = usize::from(read_u16(bytes, coff + 16, "COFF SizeOfOptionalHeader")?);
    let optional_end = optional
        .checked_add(optional_size)
        .context("PE optional header size overflow")?;
    ensure!(
        optional_end <= bytes.len(),
        "PE optional header extends past EOF"
    );

    let magic = read_u16(bytes, optional, "optional-header magic")?;
    let (directory_count_offset, directories_offset) = match magic {
        0x010b => (92_usize, 96_usize),
        0x020b => (108_usize, 112_usize),
        _ => bail!("unsupported PE optional-header magic 0x{magic:04x}"),
    };
    let checksum_offset = optional
        .checked_add(64)
        .context("PE checksum offset overflow")?;
    let directory_count_position = optional
        .checked_add(directory_count_offset)
        .context("PE directory count offset overflow")?;
    let security_directory_offset = optional
        .checked_add(directories_offset)
        .and_then(|offset| offset.checked_add(IMAGE_DIRECTORY_ENTRY_SECURITY.checked_mul(8)?))
        .context("PE security directory offset overflow")?;
    ensure!(
        checksum_offset + 4 <= optional_end
            && directory_count_position + 4 <= optional_end
            && security_directory_offset + 8 <= optional_end,
        "PE optional header is too short for Authenticode fields"
    );
    let directory_count =
        read_u32(bytes, directory_count_position, "NumberOfRvaAndSizes")? as usize;
    ensure!(
        directory_count > IMAGE_DIRECTORY_ENTRY_SECURITY,
        "PE optional header does not declare a security directory"
    );

    let size_of_headers = read_u32(bytes, optional + 60, "optional-header SizeOfHeaders")? as usize;
    let section_table_size = section_count
        .checked_mul(40)
        .context("PE section table size overflow")?;
    let section_table_end = optional_end
        .checked_add(section_table_size)
        .context("PE section table end overflow")?;
    ensure!(
        section_table_end <= bytes.len() && section_table_end <= size_of_headers,
        "PE section table is truncated or outside SizeOfHeaders"
    );
    ensure!(
        size_of_headers <= bytes.len(),
        "PE SizeOfHeaders extends past EOF"
    );

    let mut image_end = size_of_headers;
    for index in 0..section_count {
        let section = optional_end + index * 40;
        let raw_size = read_u32(bytes, section + 16, "section SizeOfRawData")? as usize;
        let raw_offset = read_u32(bytes, section + 20, "section PointerToRawData")? as usize;
        if raw_size == 0 {
            continue;
        }
        ensure!(
            raw_offset >= size_of_headers,
            "PE section raw data overlaps its headers"
        );
        let raw_end = raw_offset
            .checked_add(raw_size)
            .context("PE section raw extent overflow")?;
        ensure!(
            raw_end <= bytes.len(),
            "PE section raw data extends past EOF"
        );
        image_end = image_end.max(raw_end);
    }

    let certificate_offset = read_u32(
        bytes,
        security_directory_offset,
        "security-directory file offset",
    )? as usize;
    let certificate_size = read_u32(
        bytes,
        security_directory_offset + 4,
        "security-directory size",
    )? as usize;
    let certificate = match (certificate_offset, certificate_size) {
        (0, 0) => None,
        (0, _) | (_, 0) => bail!("PE security directory has an inconsistent zero offset/size"),
        (offset, size) => {
            ensure!(
                offset.is_multiple_of(8),
                "PE certificate table is not 8-byte aligned"
            );
            ensure!(
                offset >= image_end,
                "PE certificate table overlaps headers or section data"
            );
            let end = offset
                .checked_add(size)
                .context("PE certificate table extent overflow")?;
            ensure!(
                end == bytes.len(),
                "PE certificate table must be the exact terminal file region"
            );
            validate_certificate_table(bytes, offset, end)?;
            Some(CertificateTable { offset })
        }
    };

    Ok(PeLayout {
        checksum_offset,
        security_directory_offset,
        certificate,
    })
}

fn validate_certificate_table(bytes: &[u8], start: usize, end: usize) -> Result<()> {
    ensure!(
        end - start >= 8,
        "PE certificate table is shorter than its header"
    );
    let mut cursor = start;
    let mut entries = 0_usize;
    while cursor < end {
        ensure!(
            end - cursor >= 8,
            "truncated WIN_CERTIFICATE header or alignment padding"
        );
        let length = read_u32(bytes, cursor, "WIN_CERTIFICATE.dwLength")? as usize;
        ensure!(
            length > 8,
            "WIN_CERTIFICATE has an empty or invalid payload length"
        );
        let revision = read_u16(bytes, cursor + 4, "WIN_CERTIFICATE.wRevision")?;
        let certificate_type = read_u16(bytes, cursor + 6, "WIN_CERTIFICATE.wCertificateType")?;
        ensure!(
            revision == WIN_CERT_REVISION_2_0,
            "WIN_CERTIFICATE has unsupported revision 0x{revision:04x}"
        );
        ensure!(
            certificate_type == WIN_CERT_TYPE_PKCS_SIGNED_DATA,
            "WIN_CERTIFICATE is not PKCS#7 signed data"
        );
        let unaligned_end = cursor
            .checked_add(length)
            .context("WIN_CERTIFICATE length overflow")?;
        let aligned_length = length
            .checked_add(7)
            .map(|value| value & !7)
            .context("WIN_CERTIFICATE alignment overflow")?;
        let aligned_end = cursor
            .checked_add(aligned_length)
            .context("WIN_CERTIFICATE aligned extent overflow")?;
        ensure!(
            unaligned_end <= aligned_end && aligned_end <= end,
            "WIN_CERTIFICATE extends past its declared table"
        );
        ensure!(
            bytes[unaligned_end..aligned_end]
                .iter()
                .all(|byte| *byte == 0),
            "WIN_CERTIFICATE has non-zero alignment padding"
        );
        entries += 1;
        cursor = aligned_end;
    }
    ensure!(
        cursor == end,
        "WIN_CERTIFICATE parsing did not consume the complete table"
    );
    ensure!(
        entries == 1,
        "PE must contain exactly one WIN_CERTIFICATE entry, found {entries}"
    );
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize, label: &str) -> Result<u16> {
    let raw: [u8; 2] = bytes
        .get(offset..offset.saturating_add(2))
        .with_context(|| format!("truncated {label}"))?
        .try_into()
        .with_context(|| format!("invalid {label} width"))?;
    Ok(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize, label: &str) -> Result<u32> {
    let raw: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .with_context(|| format!("truncated {label}"))?
        .try_into()
        .with_context(|| format!("invalid {label} width"))?;
    Ok(u32::from_le_bytes(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SCRATCH_ID: AtomicU64 = AtomicU64::new(0);
    const FIXTURE_SOURCE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER_SOURCE_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn scratch(tag: &str) -> PathBuf {
        let id = SCRATCH_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "xtask-bundle-seal-{tag}-{}-{id}",
            std::process::id()
        ))
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn unsigned_pe(marker: u8) -> Vec<u8> {
        const PE: usize = 0x80;
        const COFF: usize = PE + 4;
        const OPTIONAL: usize = COFF + 20;
        const OPTIONAL_SIZE: usize = 0xf0;
        const SECTION: usize = OPTIONAL + OPTIONAL_SIZE;
        let mut bytes = vec![0_u8; 0x400];
        bytes[..2].copy_from_slice(b"MZ");
        put_u32(&mut bytes, 0x3c, PE as u32);
        bytes[PE..PE + 4].copy_from_slice(b"PE\0\0");
        put_u16(&mut bytes, COFF, 0x8664);
        put_u16(&mut bytes, COFF + 2, 1);
        put_u16(&mut bytes, COFF + 16, OPTIONAL_SIZE as u16);
        put_u16(&mut bytes, OPTIONAL, 0x020b);
        put_u32(&mut bytes, OPTIONAL + 56, 0x2000);
        put_u32(&mut bytes, OPTIONAL + 60, 0x200);
        put_u32(&mut bytes, OPTIONAL + 64, 0x1234_5678);
        put_u32(&mut bytes, OPTIONAL + 108, 16);
        bytes[SECTION..SECTION + 5].copy_from_slice(b".text");
        put_u32(&mut bytes, SECTION + 16, 0x200);
        put_u32(&mut bytes, SECTION + 20, 0x200);
        bytes[0x200..].fill(marker);
        bytes
    }

    fn sign_pe(unsigned: &[u8], certificate_entries: usize) -> Vec<u8> {
        const OPTIONAL: usize = 0x98;
        const SECURITY: usize = OPTIONAL + 144;
        let mut bytes = unsigned.to_vec();
        while !bytes.len().is_multiple_of(8) {
            bytes.push(0);
        }
        let certificate_offset = bytes.len();
        put_u32(&mut bytes, OPTIONAL + 64, 0xaabb_ccdd);
        for index in 0..certificate_entries {
            let start = bytes.len();
            bytes.resize(start + 16, 0);
            put_u32(&mut bytes, start, 12);
            put_u16(&mut bytes, start + 4, WIN_CERT_REVISION_2_0);
            put_u16(&mut bytes, start + 6, WIN_CERT_TYPE_PKCS_SIGNED_DATA);
            bytes[start + 8..start + 12].copy_from_slice(&[0x30, 0x02, index as u8, 0]);
        }
        put_u32(&mut bytes, SECURITY, certificate_offset as u32);
        let certificate_size = (bytes.len() - certificate_offset) as u32;
        put_u32(&mut bytes, SECURITY + 4, certificate_size);
        bytes
    }

    fn write_bundle(root: &Path, signed: bool) {
        fs::create_dir_all(root).expect("create fixture root");
        for (index, (relative, _)) in FIRST_PARTY_PES.iter().enumerate() {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("fixture PE parent"))
                .expect("create fixture PE parent");
            let unsigned = unsigned_pe(index as u8 + 1);
            let bytes = if signed {
                sign_pe(&unsigned, 1)
            } else {
                unsigned
            };
            fs::write(path, bytes).expect("write fixture PE");
        }
        fs::write(root.join("README.txt"), b"release notes").expect("write fixture README");
        let buildinfo = format!(
            "\u{feff}FindMyFiles\r\nversion:  1.2.3\r\nchannel:  stable\r\n\
             commit:   {FIXTURE_SOURCE_COMMIT}\r\n"
        );
        fs::write(root.join("BUILDINFO.txt"), buildinfo).expect("write fixture BUILDINFO");
    }

    fn manifest_file(base: &Path, state: BundleState) -> PathBuf {
        base.join(match state {
            BundleState::Unsigned => "unsigned.json",
            BundleState::Signed => "signed.json",
        })
    }

    #[test]
    fn manifest_is_deterministic_and_round_trips_exactly() {
        let base = scratch("deterministic");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);

        seal_to(&root, &manifest, BundleState::Unsigned).expect("first seal");
        let first = fs::read(&manifest).expect("read first manifest");
        seal_to(&root, &manifest, BundleState::Unsigned).expect("second seal");
        let second = fs::read(&manifest).expect("read second manifest");

        assert_eq!(first, second);
        verify_at(&root, &manifest, BundleState::Unsigned).expect("verify exact unsigned bundle");
        let parsed = read_manifest(&manifest).expect("parse deterministic manifest");
        assert_eq!(parsed.source_commit, FIXTURE_SOURCE_COMMIT);
        assert!(parsed
            .files
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path));
        assert_eq!(
            parsed
                .files
                .iter()
                .filter(|file| file.pe_payload_sha256.is_some())
                .count(),
            FIRST_PARTY_PES.len()
        );
        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }

    #[test]
    fn source_commit_is_bound_both_to_buildinfo_and_the_manifest() {
        let base = scratch("source-commit-tamper");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);
        seal_to(&root, &manifest, BundleState::Unsigned).expect("seal fixture");

        let buildinfo_path = root.join("BUILDINFO.txt");
        let original_buildinfo =
            fs::read_to_string(&buildinfo_path).expect("read fixture BUILDINFO");
        fs::write(
            &buildinfo_path,
            original_buildinfo.replacen(FIXTURE_SOURCE_COMMIT, OTHER_SOURCE_COMMIT, 1),
        )
        .expect("tamper BUILDINFO source commit");
        let buildinfo_error = verify_at(&root, &manifest, BundleState::Unsigned)
            .expect_err("BUILDINFO source-commit tamper must fail")
            .to_string();
        assert!(buildinfo_error.contains("bundle source commit differs"));
        fs::write(&buildinfo_path, original_buildinfo).expect("restore fixture BUILDINFO");

        let original_manifest = fs::read_to_string(&manifest).expect("read fixture manifest");
        fs::write(
            &manifest,
            original_manifest.replacen(FIXTURE_SOURCE_COMMIT, OTHER_SOURCE_COMMIT, 1),
        )
        .expect("tamper manifest source commit");
        let manifest_error = verify_at(&root, &manifest, BundleState::Unsigned)
            .expect_err("manifest source-commit tamper must fail")
            .to_string();
        assert!(manifest_error.contains("bundle source commit differs"));

        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }

    #[test]
    fn manifest_source_commit_rejects_empty_uppercase_and_duplicate_fields() {
        let base = scratch("invalid-source-commit");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);
        seal_to(&root, &manifest, BundleState::Unsigned).expect("seal fixture");
        let original = fs::read_to_string(&manifest).expect("read fixture manifest");

        for invalid in ["", "0123456789ABCDEF0123456789ABCDEF01234567"] {
            fs::write(
                &manifest,
                original.replacen(FIXTURE_SOURCE_COMMIT, invalid, 1),
            )
            .expect("write invalid source commit");
            assert!(read_manifest(&manifest).is_err(), "{invalid:?}");
        }

        let field = format!("  \"source_commit\": \"{FIXTURE_SOURCE_COMMIT}\",");
        let duplicate = original.replacen(&field, &format!("{field}\n{field}"), 1);
        fs::write(&manifest, duplicate).expect("write duplicate source_commit field");
        assert!(read_manifest(&manifest).is_err());

        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }

    #[test]
    fn windows_device_aliases_include_superscript_digits_and_extensions() {
        for reserved in [
            "CON",
            "nul.txt",
            "COM1",
            "com9.log",
            "LPT1",
            "lpt9.dll",
            "COM¹",
            "COM¹.txt",
            "com².txt",
            "COM³.txt",
            "LPT¹",
            "LPT¹.dll",
            "lpt².log",
            "LpT³.dll",
            "app/COM¹.txt",
        ] {
            assert!(
                validate_relative_path(reserved).is_err(),
                "{reserved:?} must be rejected"
            );
        }

        for safe in [
            "COM0",
            "COM10.txt",
            "LPT0",
            "LPT10.log",
            "COM-one.txt",
            "app/README.txt",
        ] {
            validate_relative_path(safe).unwrap();
        }
    }

    #[test]
    fn non_ascii_paths_fail_at_manifest_tree_and_consumer_boundaries() {
        for unsafe_path in ["résumé.txt", "日本語.txt", "app/β.dll", "app/emoji-😀.txt"] {
            assert!(
                validate_relative_path(unsafe_path).is_err(),
                "{unsafe_path:?} must be rejected"
            );
        }

        let base = scratch("non-ascii");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);
        seal_to(&root, &manifest, BundleState::Unsigned).expect("seal ASCII fixture");
        fs::write(root.join("résumé.txt"), b"unexpected payload").expect("write non-ASCII fixture");

        assert!(verify_at(&root, &manifest, BundleState::Unsigned).is_err());
        assert!(collect_bundle_files(&root, BundleState::Unsigned).is_err());

        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }

    #[test]
    fn exact_verification_rejects_extra_missing_and_hash_drift() {
        for case in ["extra", "missing", "drift"] {
            let base = scratch(case);
            let root = base.join("bundle");
            let manifest = manifest_file(&base, BundleState::Unsigned);
            write_bundle(&root, false);
            seal_to(&root, &manifest, BundleState::Unsigned).expect("seal fixture");

            match case {
                "extra" => fs::write(root.join("extra.txt"), b"surprise").expect("write extra"),
                "missing" => {
                    fs::remove_file(root.join("README.txt")).expect("remove expected file");
                }
                "drift" => fs::write(root.join("README.txt"), b"changed").expect("change file"),
                _ => unreachable!("closed case list"),
            }
            assert!(verify_at(&root, &manifest, BundleState::Unsigned).is_err());
            crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
        }
    }

    #[test]
    fn exact_unsigned_verification_rejects_checksum_only_tamper() {
        let base = scratch("checksum-tamper");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);
        seal_to(&root, &manifest, BundleState::Unsigned).expect("seal fixture");

        let target = root.join(FIRST_PARTY_PES[0].0);
        let mut bytes = fs::read(&target).expect("read target");
        put_u32(&mut bytes, 0x98 + 64, 0xfeed_beef);
        fs::write(&target, bytes).expect("tamper checksum");
        assert!(verify_at(&root, &manifest, BundleState::Unsigned).is_err());
        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }

    #[test]
    fn signed_transition_allows_only_checksum_directory_and_one_certificate() {
        let base = scratch("transition");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);
        seal_to(&root, &manifest, BundleState::Unsigned).expect("seal unsigned");
        for (relative, _) in FIRST_PARTY_PES {
            let path = root.join(relative);
            let unsigned = fs::read(&path).expect("read unsigned target");
            fs::write(&path, sign_pe(&unsigned, 1)).expect("write signed target");
        }

        verify_signed_transition_at(&root, &manifest).expect("verify signing-only transition");
        let signed_manifest = manifest_file(&base, BundleState::Signed);
        seal_to(&root, &signed_manifest, BundleState::Signed).expect("seal signed");
        verify_at(&root, &signed_manifest, BundleState::Signed)
            .expect("verify exact signed bundle");
        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }

    #[test]
    fn signed_transition_rejects_payload_and_overlay_tampering() {
        for case in ["payload", "overlay"] {
            let base = scratch(case);
            let root = base.join("bundle");
            let manifest = manifest_file(&base, BundleState::Unsigned);
            write_bundle(&root, false);
            if case == "overlay" {
                for (relative, _) in FIRST_PARTY_PES {
                    let path = root.join(relative);
                    let mut bytes = fs::read(&path).expect("read PE for overlay");
                    bytes.extend_from_slice(b"OVERLAY!");
                    fs::write(path, bytes).expect("add unsigned overlay");
                }
            }
            seal_to(&root, &manifest, BundleState::Unsigned).expect("seal unsigned");
            for (index, (relative, _)) in FIRST_PARTY_PES.iter().enumerate() {
                let path = root.join(relative);
                let mut unsigned = fs::read(&path).expect("read unsigned target");
                if index == 0 {
                    let offset = if case == "payload" {
                        0x220
                    } else {
                        unsigned.len() - 1
                    };
                    unsigned[offset] ^= 0x5a;
                }
                fs::write(path, sign_pe(&unsigned, 1)).expect("write signed target");
            }
            assert!(verify_signed_transition_at(&root, &manifest).is_err());
            crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
        }
    }

    #[test]
    fn transition_rejects_unchanged_target_and_changed_non_target() {
        for case in ["unchanged-target", "changed-other"] {
            let base = scratch(case);
            let root = base.join("bundle");
            let manifest = manifest_file(&base, BundleState::Unsigned);
            write_bundle(&root, false);
            seal_to(&root, &manifest, BundleState::Unsigned).expect("seal unsigned");
            for (index, (relative, _)) in FIRST_PARTY_PES.iter().enumerate() {
                if case == "unchanged-target" && index == 0 {
                    continue;
                }
                let path = root.join(relative);
                let unsigned = fs::read(&path).expect("read unsigned target");
                fs::write(path, sign_pe(&unsigned, 1)).expect("write signed target");
            }
            if case == "changed-other" {
                fs::write(root.join("README.txt"), b"tampered").expect("tamper non-target");
            }
            assert!(verify_signed_transition_at(&root, &manifest).is_err());
            crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
        }
    }

    #[test]
    fn malformed_pe_and_certificate_layouts_are_rejected() {
        let mut malformed = unsigned_pe(1);
        malformed[0] = 0;
        assert!(normalized_pe_payload_sha256(&malformed, BundleState::Unsigned).is_err());

        let unsigned = unsigned_pe(1);
        let multiple = sign_pe(&unsigned, 2);
        assert!(normalized_pe_payload_sha256(&multiple, BundleState::Signed).is_err());

        let mut non_terminal = sign_pe(&unsigned, 1);
        non_terminal.push(0);
        assert!(normalized_pe_payload_sha256(&non_terminal, BundleState::Signed).is_err());

        let mut overlapping = sign_pe(&unsigned, 1);
        put_u32(&mut overlapping, 0x98 + 144, 0x200);
        let overlapping_size = (overlapping.len() - 0x200) as u32;
        put_u32(&mut overlapping, 0x98 + 148, overlapping_size);
        assert!(normalized_pe_payload_sha256(&overlapping, BundleState::Signed).is_err());

        let mut bad_padding = sign_pe(&unsigned, 1);
        let last = bad_padding.len() - 1;
        bad_padding[last] = 1;
        assert!(normalized_pe_payload_sha256(&bad_padding, BundleState::Signed).is_err());
    }

    #[test]
    fn manifest_rejects_case_collisions_traversal_duplicates_and_unknown_fields() {
        let hash = checksum::sha256_hex(b"x");
        let manifest = BundleManifest {
            schema_version: SCHEMA_VERSION,
            source_commit: FIXTURE_SOURCE_COMMIT.to_owned(),
            state: BundleState::Unsigned,
            files: vec![
                BundleFileIdentity {
                    path: "README.TXT".to_owned(),
                    size: 1,
                    sha256: hash.clone(),
                    pe_payload_sha256: None,
                    is_pe: false,
                },
                BundleFileIdentity {
                    path: "Readme.txt".to_owned(),
                    size: 1,
                    sha256: hash.clone(),
                    pe_payload_sha256: None,
                    is_pe: false,
                },
            ],
        };
        assert!(validate_manifest(&manifest).is_err());
        let duplicate_paths = BundleManifest {
            schema_version: SCHEMA_VERSION,
            source_commit: FIXTURE_SOURCE_COMMIT.to_owned(),
            state: BundleState::Unsigned,
            files: vec![
                BundleFileIdentity {
                    path: "README.txt".to_owned(),
                    size: 1,
                    sha256: hash.clone(),
                    pe_payload_sha256: None,
                    is_pe: false,
                },
                BundleFileIdentity {
                    path: "README.txt".to_owned(),
                    size: 1,
                    sha256: hash,
                    pe_payload_sha256: None,
                    is_pe: false,
                },
            ],
        };
        assert!(validate_manifest(&duplicate_paths).is_err());
        assert!(validate_relative_path("../escape").is_err());
        assert!(validate_relative_path("app\\escape").is_err());

        let duplicate_field = br#"{
          "schema_version": 2,
          "schema_version": 2,
          "source_commit": "0123456789abcdef0123456789abcdef01234567",
          "state": "unsigned",
          "files": []
        }"#;
        assert!(serde_json::from_slice::<BundleManifest>(duplicate_field).is_err());
        let unknown_field = br#"{
          "schema_version": 2,
          "source_commit": "0123456789abcdef0123456789abcdef01234567",
          "state": "unsigned",
          "surprise": true,
          "files": []
        }"#;
        assert!(serde_json::from_slice::<BundleManifest>(unknown_field).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let base = scratch("symlink");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);
        symlink(root.join("README.txt"), root.join("link.txt")).expect("create fixture symlink");
        assert!(seal_to(&root, &manifest, BundleState::Unsigned).is_err());
        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }

    #[cfg(windows)]
    #[test]
    fn symlinks_or_reparse_points_are_rejected_when_creation_is_available() {
        use std::os::windows::fs::symlink_file;

        let base = scratch("symlink");
        let root = base.join("bundle");
        let manifest = manifest_file(&base, BundleState::Unsigned);
        write_bundle(&root, false);
        if symlink_file(root.join("README.txt"), root.join("link.txt")).is_ok() {
            assert!(seal_to(&root, &manifest, BundleState::Unsigned).is_err());
        }
        crate::fsx::force_remove_dir_all(&base).expect("clean fixture");
    }
}
