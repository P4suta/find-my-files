//! Elevated adversarial filesystem tests for the `LocalSystem` install boundary.
//!
//! Gated like every machine-security test: `#[ignore]` plus
//! `FMF_ADMIN_TESTS=1` (`just test-admin`, from an elevated shell).

use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::FromRawHandle as _;

use fmf_service::security::{TrustedDataRoot, TrustedSourceFile, data_dir_sddl};

const FILE_ADD_FILE: u32 = 0x0000_0002;
const FILE_DELETE_CHILD: u32 = 0x0000_0040;
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
const DELETE: u32 = 0x0001_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;

/// Fails closed. `#[ignore]` is what *skips* these tests; reaching the body
/// without the arming variable means the harness was invoked outside
/// `just test-admin`, and a silent early return would be indistinguishable
/// from a proven boundary.
fn require_admin_gate() {
    assert_eq!(
        std::env::var("FMF_ADMIN_TESTS").as_deref(),
        Ok("1"),
        "this ignored machine-security test must run only through `just test-admin`"
    );
}

fn open_directory_handle(path: &std::path::Path, access: u32) -> std::fs::File {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        handle,
        INVALID_HANDLE_VALUE,
        "open mutation handle: {}",
        std::io::Error::last_os_error()
    );
    unsafe { std::fs::File::from_raw_handle(handle.cast()) }
}

#[test]
#[ignore = "requires elevation/symlink privilege; gated by FMF_ADMIN_TESTS=1"]
fn reparse_and_preopened_mutation_handles_fail_closed() {
    require_admin_gate();
    let sddl = data_dir_sddl();

    // A root reparse point must be opened as the link itself and rotated out of
    // the privileged fixed name. The target is never traversed or modified.
    let root_case = tempfile::tempdir().expect("root-case anchor");
    let root_target = tempfile::tempdir().expect("root-case target");
    let root_sentinel = root_target.path().join("sentinel");
    std::fs::write(&root_sentinel, b"outside-root").expect("root sentinel");
    let root = root_case.path().join("find-my-files");
    std::os::windows::fs::symlink_dir(root_target.path(), &root).expect("root symlink");
    let trusted = TrustedDataRoot::create_or_replace_for_test(&root, &sddl)
        .expect("root reparse is rotated without being followed");
    let provenance = trusted.provenance_for_test();
    let quarantined_reparse = trusted
        .quarantined_root()
        .expect("reparse quarantine path")
        .to_path_buf();
    drop(trusted);
    assert_eq!(
        std::fs::read(&root_sentinel).expect("outside root remains readable"),
        b"outside-root"
    );
    std::fs::remove_dir(&quarantined_reparse).expect("remove quarantined root symlink");

    // A descendant reparse point is found while the real root handle remains
    // locked. No inheritable ACL is propagated until traversal has completed.
    let descendant_target = tempfile::tempdir().expect("descendant target");
    let descendant_sentinel = descendant_target.path().join("sentinel");
    std::fs::write(&descendant_sentinel, b"outside-child").expect("child sentinel");
    std::os::windows::fs::symlink_dir(descendant_target.path(), root.join("index"))
        .expect("index symlink");
    let error = TrustedDataRoot::open_verified_for_test(&root, provenance, &sddl)
        .expect_err("descendant reparse must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        std::fs::read(&descendant_sentinel).expect("outside child remains readable"),
        b"outside-child"
    );
    std::fs::remove_dir(root.join("index")).expect("remove index symlink");

    // An attacker retaining FILE_ADD_FILE/FILE_DELETE_CHILD before validation
    // must cause a sharing violation. Changing a DACL would not revoke that
    // already-open handle, so the object is never accepted in that state.
    let mutation = open_directory_handle(&root, FILE_ADD_FILE | FILE_DELETE_CHILD);
    let error = TrustedDataRoot::open_verified_for_test(&root, provenance, &sddl)
        .expect_err("mutation handle must block");
    assert_eq!(error.raw_os_error(), Some(32), "{error}");
    drop(mutation);

    // Once no mutation handle exists, the same root can be pinned. Its live
    // guard prevents a remove→symlink swap, and handle-relative publication
    // writes only below the verified root.
    let trusted =
        TrustedDataRoot::open_verified_for_test(&root, provenance, &sddl).expect("trusted root");
    let swap_target = tempfile::tempdir().expect("swap target");
    let swap_sentinel = swap_target.path().join("sentinel");
    std::fs::write(&swap_sentinel, b"outside-swap").expect("swap sentinel");
    let remove_error = std::fs::remove_dir(&root).expect_err("locked root cannot be removed");
    assert_eq!(remove_error.raw_os_error(), Some(32), "{remove_error}");
    assert!(
        std::os::windows::fs::symlink_dir(swap_target.path(), &root).is_err(),
        "the occupied fixed name cannot be replaced by a symlink"
    );

    trusted
        .atomic_write("service.json", b"{\"locked\":true}", &sddl)
        .expect("handle-relative publication");
    assert_eq!(
        std::fs::read(root.join("service.json")).expect("published file"),
        b"{\"locked\":true}"
    );
    assert_eq!(
        std::fs::read(&swap_sentinel).expect("swap target remains untouched"),
        b"outside-swap"
    );

    // The executable source is pinned once too. A writer cannot change the
    // path after validation, and the copy duplicates that same file object
    // instead of resolving the path again.
    let source_path = root_case.path().join("source.exe");
    std::fs::write(&source_path, b"verified-image").expect("source image");
    let source = TrustedSourceFile::open(&source_path).expect("pin source image");
    let rewrite_error =
        std::fs::write(&source_path, b"attacker-image").expect_err("source write must be denied");
    assert_eq!(rewrite_error.raw_os_error(), Some(32), "{rewrite_error}");
    trusted
        .atomic_copy("fmf-service.exe", &source, &sddl)
        .expect("copy from exact source handle");
    assert_eq!(
        std::fs::read(root.join("fmf-service.exe")).expect("copied image"),
        b"verified-image"
    );

    trusted.purge().expect("exact-handle purge");
    assert!(!root.exists(), "verified root was deleted");
}

#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS=1"]
fn hard_links_are_rejected_before_acl_or_content_mutation() {
    require_admin_gate();
    let anchor = tempfile::tempdir().expect("anchor");
    let outside = tempfile::tempdir().expect("outside");
    let root = anchor.path().join("find-my-files");
    std::fs::create_dir(&root).expect("root");
    let outside_config = outside.path().join("outside.json");
    std::fs::write(&outside_config, b"outside").expect("outside file");

    let trusted =
        TrustedDataRoot::create_or_replace_for_test(&root, &data_dir_sddl()).expect("fresh root");
    let provenance = trusted.provenance_for_test();
    drop(trusted);
    std::fs::hard_link(&outside_config, root.join("service.json")).expect("hard link");

    let error = TrustedDataRoot::open_verified_for_test(&root, provenance, &data_dir_sddl())
        .expect_err("multiply-linked control file must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        std::fs::read(&outside_config).expect("outside file"),
        b"outside"
    );

    std::fs::remove_file(root.join("service.json")).expect("remove hard link");
    TrustedDataRoot::open_verified_for_test(&root, provenance, &data_dir_sddl())
        .expect("clean root")
        .purge()
        .expect("purge");
}

fn file_identity(file: &std::fs::File) -> (u32, u64) {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &raw mut info) };
    assert_ne!(ok, 0, "file identity: {}", std::io::Error::last_os_error());
    (
        info.dwVolumeSerialNumber,
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    )
}

/// Shared body of the two rotation tests. The gate stays in each `#[ignore]`
/// test rather than here, so the arming check is visible at every entry point
/// to the privileged surface instead of hiding one level down.
fn assert_preopened_security_handle_is_rotated(access: u32, label: &str) {
    let anchor = tempfile::tempdir().expect("anchor");
    let root = anchor.path().join("find-my-files");
    std::fs::create_dir(&root).expect("attacker-created root");
    let stale = open_directory_handle(&root, access);
    let stale_identity = file_identity(&stale);

    let trusted = TrustedDataRoot::create_or_replace_for_test(&root, &data_dir_sddl())
        .unwrap_or_else(|error| panic!("{label} must be neutralized by root rotation: {error}"));
    let quarantine = trusted
        .quarantined_root()
        .unwrap_or_else(|| panic!("{label} root was reused in place"));
    let current = open_directory_handle(&root, FILE_READ_ATTRIBUTES);
    let quarantined = open_directory_handle(quarantine, FILE_READ_ATTRIBUTES);

    assert_ne!(
        file_identity(&current),
        stale_identity,
        "{label} remained attached to the privileged fixed-name root"
    );
    assert_eq!(
        file_identity(&quarantined),
        stale_identity,
        "{label} must remain attached only to the quarantined old object"
    );
    trusted
        .atomic_write("service.json", b"first", &data_dir_sddl())
        .expect("first atomic publication");
    trusted
        .atomic_write("service.json", b"replacement", &data_dir_sddl())
        .expect("atomic replacement");
    assert_eq!(
        std::fs::read(root.join("service.json")).expect("replacement bytes"),
        b"replacement"
    );
    assert!(
        std::fs::read_dir(&root)
            .expect("root entries")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".fmf-stage-")),
        "atomic replacement must clean staging files"
    );
}

#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS=1"]
fn preopened_write_dac_handle_is_rotated_out_of_the_privileged_name() {
    require_admin_gate();
    assert_preopened_security_handle_is_rotated(WRITE_DAC, "WRITE_DAC");
}

#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS=1"]
fn preopened_write_owner_handle_is_rotated_out_of_the_privileged_name() {
    require_admin_gate();
    assert_preopened_security_handle_is_rotated(WRITE_OWNER, "WRITE_OWNER");
}

#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS=1"]
fn preopened_delete_handle_blocks_root_adoption() {
    require_admin_gate();
    let anchor = tempfile::tempdir().expect("anchor");
    let root = anchor.path().join("find-my-files");
    std::fs::create_dir(&root).expect("attacker-created root");
    let deletion = open_directory_handle(&root, DELETE);
    let stale_identity = file_identity(&deletion);

    let error = TrustedDataRoot::create_or_replace_for_test(&root, &data_dir_sddl())
        .expect_err("a live DELETE handle must fail closed");
    assert_eq!(error.raw_os_error(), Some(32), "{error}");
    let still_fixed = open_directory_handle(&root, FILE_READ_ATTRIBUTES);
    assert_eq!(
        file_identity(&still_fixed),
        stale_identity,
        "a blocked install must neither trust nor replace the attacker object"
    );
}

#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS=1"]
fn provenance_match_never_repairs_a_drifted_root_acl_in_place() {
    require_admin_gate();
    let anchor = tempfile::tempdir().expect("anchor");
    let root = anchor.path().join("find-my-files");
    let trusted =
        TrustedDataRoot::create_or_replace_for_test(&root, &data_dir_sddl()).expect("fresh root");
    let provenance = trusted.provenance_for_test();
    let weak = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;BU)";
    trusted
        .set_root_security(weak)
        .expect("simulate a drifted compatibility ACL");
    let stale_security = open_directory_handle(&root, WRITE_DAC);
    let stale_identity = file_identity(&stale_security);
    drop(trusted);

    let reopened = TrustedDataRoot::open_verified_for_test(&root, provenance, &data_dir_sddl())
        .expect("drifted root must be rotated, not repaired");
    let quarantine = reopened
        .quarantined_root()
        .expect("ACL drift must force quarantine");
    let current = open_directory_handle(&root, FILE_READ_ATTRIBUTES);
    let old = open_directory_handle(quarantine, FILE_READ_ATTRIBUTES);
    assert_ne!(file_identity(&current), stale_identity);
    assert_eq!(
        file_identity(&old),
        stale_identity,
        "the pre-open WRITE_DAC handle must remain attached only to quarantine"
    );
}

/// `FILE_DELETE_CHILD` is rotated out, not failed closed — and that is the
/// correct guarantee, not a concession.
///
/// Windows buckets share access into read (`FILE_READ_DATA`/`FILE_EXECUTE`),
/// write (`FILE_WRITE_DATA`/`FILE_APPEND_DATA`) and delete (`DELETE`).
/// `FILE_DELETE_CHILD` is in none of them, so a handle holding only that right
/// does not participate in sharing at all and *no* share mode can exclude it.
/// This test asserted a sharing violation and had never run; it could not have
/// passed. Its sibling holding `DELETE` — which does participate — still does
/// fail closed, so the two behaviours are genuinely different and
/// `docs/SECURITY.md` threat 7 records which right gets which.
///
/// That rotation is *sufficient* is measured, not assumed:
/// `security::tests::a_delete_child_handle_keeps_the_quarantined_object_and_never_reaches_the_new_root`
/// impersonates a real standard user and shows the capability follows the
/// object it was opened on — the squatter still deletes inside the quarantined
/// directory and is denied on the fresh root, which admits only SYSTEM and
/// Administrators.
#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS=1"]
fn preopened_delete_child_handle_is_rotated_out_of_the_privileged_name() {
    require_admin_gate();
    assert_preopened_security_handle_is_rotated(FILE_DELETE_CHILD, "FILE_DELETE_CHILD");
}

#[test]
#[ignore = "requires elevation; gated by FMF_ADMIN_TESTS=1"]
fn managed_tree_depth_is_bounded_without_recursive_stack_growth() {
    require_admin_gate();
    let anchor = tempfile::tempdir().expect("anchor");
    let root = anchor.path().join("find-my-files");
    let trusted =
        TrustedDataRoot::create_or_replace_for_test(&root, &data_dir_sddl()).expect("fresh root");
    let provenance = trusted.provenance_for_test();
    drop(trusted);

    let mut cursor = root.clone();
    for _ in 0..65 {
        cursor.push("d");
        std::fs::create_dir(&cursor).expect("deep directory");
    }
    let error = TrustedDataRoot::open_verified_for_test(&root, provenance, &data_dir_sddl())
        .expect_err("depth limit must fail closed");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("64-level"));
}
