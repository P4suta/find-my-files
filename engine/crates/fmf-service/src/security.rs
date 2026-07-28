//! Pipe DACL and token checks (docs/SECURITY.md layers 1 and 4 of the 4-layer
//! defense; rationale ADR-0017).
//!
//! The SDDL string is built by one pure, unit-pinned function — a hand-rolled
//! SDDL elsewhere is exactly the "silently wide open" accident the pin exists
//! to prevent. Never create a pipe without going through
//! `pipe_security_attributes`.

use std::io;
use std::io::Seek as _;
use std::io::Write as _;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
    ERROR_NONE_MAPPED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

/// Strict decimal SID grammar accepted in service-owned configuration and on
/// the elevation command line. Account existence/type is a separate check in
/// [`validate_user_sid`].
#[must_use]
pub fn is_canonical_sid(value: &str) -> bool {
    const MAX_IDENTIFIER_AUTHORITY: u64 = 0x0000_FFFF_FFFF_FFFF;
    const MAX_SUB_AUTHORITIES: usize = 15;

    fn canonical_decimal(component: &str) -> bool {
        !component.is_empty()
            && (component.len() == 1 || !component.starts_with('0'))
            && component.bytes().all(|byte| byte.is_ascii_digit())
    }

    if value.len() > 184 {
        return false;
    }
    let mut components = value.split('-');
    if components.next() != Some("S") || components.next() != Some("1") {
        return false;
    }
    let Some(authority) = components.next() else {
        return false;
    };
    if !canonical_decimal(authority) {
        return false;
    }
    let Ok(authority) = authority.parse::<u64>() else {
        return false;
    };
    if authority > MAX_IDENTIFIER_AUTHORITY {
        return false;
    }

    let mut count = 0usize;
    for component in components {
        count += 1;
        if count > MAX_SUB_AUTHORITIES
            || !canonical_decimal(component)
            || component.parse::<u32>().is_err()
        {
            return false;
        }
    }
    true
}

#[derive(Debug)]
enum LookupFailure {
    Resize,
    Unmapped,
}

fn classify_lookup_failure(code: u32) -> io::Result<LookupFailure> {
    match code {
        ERROR_INSUFFICIENT_BUFFER => Ok(LookupFailure::Resize),
        ERROR_NONE_MAPPED => Ok(LookupFailure::Unmapped),
        other => Err(io::Error::from_raw_os_error(other as i32)),
    }
}

/// `D:P(A;;GA;;;SY)(A;;GRGW;;;<sid>)…` — SYSTEM gets full control, each
/// authorized SID read+write, nobody else (protected DACL, no inheritance,
/// no Everyone/anonymous ACE → default deny).
///
/// Administrators is deliberately absent: a UAC-filtered token carries it
/// deny-only and would not gain access anyway (docs/RESEARCH.md).
#[must_use]
pub fn pipe_sddl(authorized_sids: &[String]) -> String {
    let mut s = String::from("D:P(A;;GA;;;SY)");
    for sid in authorized_sids {
        s.push_str("(A;;GRGW;;;");
        s.push_str(sid);
        s.push(')');
    }
    s
}

/// Owns the security descriptor `LocalAlloc`'d by the SDDL conversion; the
/// `SECURITY_ATTRIBUTES` it hands out stays valid for its lifetime.
pub struct PipeSecurity {
    descriptor: *mut core::ffi::c_void,
}

// The descriptor is an opaque, immutable blob after creation.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// # Errors
    /// Returns the OS error if the SDDL string fails to convert to a security
    /// descriptor (`ConvertStringSecurityDescriptorToSecurityDescriptorW`).
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut descriptor: *mut core::ffi::c_void = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                (&raw mut descriptor).cast(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(Self { descriptor })
    }

    /// A `SECURITY_ATTRIBUTES` pointing at this owned descriptor, ready to pass
    /// to pipe creation. The handle is non-inheritable; valid only while `self`
    /// lives.
    #[must_use]
    pub const fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe { LocalFree(self.descriptor) };
    }
}

const DELETE_ACCESS: u32 = 0x0001_0000;
const READ_CONTROL_ACCESS: u32 = 0x0002_0000;
const WRITE_DAC_ACCESS: u32 = 0x0004_0000;
const WRITE_OWNER_ACCESS: u32 = 0x0008_0000;
const GENERIC_READ_ACCESS: u32 = 0x8000_0000;
const GENERIC_WRITE_ACCESS: u32 = 0x4000_0000;
const FILE_LIST_DIRECTORY_ACCESS: u32 = 0x0000_0001;
const FILE_ADD_FILE_ACCESS: u32 = 0x0000_0002;
const FILE_ADD_SUBDIRECTORY_ACCESS: u32 = 0x0000_0004;
const FILE_TRAVERSE_ACCESS: u32 = 0x0000_0020;
const FILE_READ_ATTRIBUTES_ACCESS: u32 = 0x0000_0080;
/// `NtCreateFile` grants no implicit synchronization. Without this right the
/// handle cannot be waited on and `FILE_SYNCHRONOUS_IO_NONALERT` is rejected,
/// so every relative open asks for it on top of the caller's mask.
const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
const SECURITY_WRITE_ACCESS: u32 =
    READ_CONTROL_ACCESS | WRITE_DAC_ACCESS | WRITE_OWNER_ACCESS | FILE_READ_ATTRIBUTES_ACCESS;
const MAX_MANAGED_TREE_DEPTH: usize = 64;
const MAX_MANAGED_TREE_OBJECTS: usize = 100_000;
const PROVENANCE_KEY: &str = r"SOFTWARE\find-my-files";
const PROVENANCE_VALUE: &str = "DataRootIdentityV1";
const PROVENANCE_MAGIC: &[u8; 4] = b"FMR1";

static STAGING_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObjectKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObjectIdentity {
    volume_serial: u32,
    file_id: u64,
}

#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Opaque exact-object identity used only by the arbitrary-parent test seam.
pub struct TestRootProvenance(ObjectIdentity);

#[derive(Debug)]
struct LockedObject {
    file: std::fs::File,
    identity: ObjectIdentity,
    kind: ObjectKind,
}

/// An exact, non-reparse, singly-linked source file whose write/delete sharing
/// is excluded for the lifetime of this guard.
#[derive(Debug)]
pub struct TrustedSourceFile(LockedObject);

impl TrustedSourceFile {
    /// Pins a regular source file before a privileged copy.
    ///
    /// # Errors
    /// Returns `InvalidData` for a reparse point/type mismatch/hard link,
    /// `SharingViolation` when a writer/deleter already has the file open, or
    /// the underlying Win32 error.
    pub fn open(path: &Path) -> io::Result<Self> {
        open_checked(
            path,
            Some(ObjectKind::File),
            GENERIC_READ_ACCESS | FILE_READ_ATTRIBUTES_ACCESS,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
        )
        .map(Self)
    }
}

/// A verified, pinned `%ProgramData%\find-my-files` tree.
///
/// Construction opens the Known-Folder-resolved `ProgramData` directory and
/// the fixed `find-my-files` child with `FILE_FLAG_OPEN_REPARSE_POINT`. The
/// construction first excludes write/delete sharing so every pre-existing data
/// or delete mutation handle fails closed. After the exact owner/group/DACL is
/// installed and read back, the same object identity is reopened for operation
/// with write sharing (needed by child renames) but still without delete
/// sharing. The protected DACL denies unprivileged writers, while the live
/// delete lease prevents replacement of the fixed root name. Security-only
/// handles are neutralized by requiring protected object-identity provenance or
/// rotating the old object out of the privileged fixed name.
///
/// Existing descendants are likewise locked before their owner/group/DACL is
/// changed on that same handle. Reparse points and multiply-linked files are
/// rejected rather than followed.
#[derive(Debug)]
pub struct TrustedDataRoot {
    root: LockedObject,
    program_data_guard: std::fs::File,
    path: PathBuf,
    quarantined_root: Option<PathBuf>,
}

impl TrustedDataRoot {
    /// Creates or opens the fixed Known-Folder-resolved machine data root.
    ///
    /// An existing object is reused only when its NTFS identity matches the
    /// identity pinned in the protected HKLM provenance key. A provenance-less
    /// object is renamed out of the privileged fixed name without modifying its
    /// ACL or walking its descendants; a fresh protected directory is then
    /// created. This is deliberately stricter than repairing an arbitrary
    /// pre-existing `ProgramData` child in place: changing a DACL does not revoke
    /// a standard user's already-open `WRITE_DAC`/`WRITE_OWNER` handle.
    ///
    /// # Errors
    /// Returns Known Folder, registry, reparse/type/link, sharing, or ACL errors.
    pub fn create_or_harden_machine(sddl: &str) -> io::Result<Self> {
        let path = crate::config::default_data_dir()?;
        let expected = read_root_provenance()?;
        Self::create_or_harden_at(&path, sddl, expected, true)
    }

    /// Opens the existing fixed machine root only when its current NTFS object
    /// identity matches the protected HKLM provenance record.
    ///
    /// # Errors
    /// Returns `NotFound` for an absent root, `PermissionDenied` for missing or
    /// mismatched provenance, or the same fail-closed errors as
    /// [`Self::create_or_harden_machine`].
    pub fn open_and_harden_machine(sddl: &str) -> io::Result<Self> {
        let path = crate::config::default_data_dir()?;
        let expected = read_root_provenance()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "machine data root has no protected provenance record",
            )
        })?;
        let (program_data_path, _) = validate_machine_root_path(&path)?;
        let program_data = open_checked(
            program_data_path,
            Some(ObjectKind::Directory),
            FILE_READ_ATTRIBUTES_ACCESS | FILE_ADD_SUBDIRECTORY_ACCESS | FILE_TRAVERSE_ACCESS,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
        )?;
        let root = open_root(&path)?;
        if root.identity != expected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "machine data root identity does not match protected provenance",
            ));
        }
        if !handle_security_matches(&root.file, sddl)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "machine data root has an unexpected owner/group/DACL",
            ));
        }
        Self::lock_and_harden(&path, program_data.file, root, sddl, None)
    }

    fn create_or_harden_at(
        path: &Path,
        sddl: &str,
        expected: Option<ObjectIdentity>,
        persist_provenance: bool,
    ) -> io::Result<Self> {
        let (program_data_path, _) = validate_machine_root_path(path)?;
        let program_data = open_checked(
            program_data_path,
            Some(ObjectKind::Directory),
            FILE_READ_ATTRIBUTES_ACCESS | FILE_ADD_SUBDIRECTORY_ACCESS | FILE_TRAVERSE_ACCESS,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
        )?;
        let (root, quarantined_root) = match open_root(path) {
            Ok(root) => {
                // Provenance identifies the object, but it does not prove that
                // its security descriptor still excludes an untrusted principal.
                // Never repair a drifted root in place: a WRITE_DAC/WRITE_OWNER
                // handle opened while it was weak would survive that repair.
                let reusable =
                    expected == Some(root.identity) && handle_security_matches(&root.file, sddl)?;
                if reusable {
                    (root, None)
                } else {
                    let quarantined_leaf =
                        quarantine_untrusted_root(root.file, &program_data.file)?;
                    (
                        create_root_from_staging(path, sddl, &program_data.file)?,
                        Some(program_data_path.join(quarantined_leaf)),
                    )
                }
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                // A reparse point or wrong-kind object is still safe to remove
                // from the privileged fixed name: open the link/object itself,
                // rename that exact handle, and never inspect its descendants.
                let untrusted = open_untrusted_object_for_quarantine(path)?;
                let quarantined_leaf = quarantine_untrusted_root(untrusted, &program_data.file)?;
                (
                    create_root_from_staging(path, sddl, &program_data.file)?,
                    Some(program_data_path.join(quarantined_leaf)),
                )
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (
                create_root_from_staging(path, sddl, &program_data.file)?,
                None,
            ),
            Err(error) => return Err(error),
        };
        let trusted = Self::lock_and_harden(path, program_data.file, root, sddl, quarantined_root)?;
        if persist_provenance {
            write_root_provenance(trusted.root.identity)?;
        }
        Ok(trusted)
    }

    fn lock_and_harden(
        path: &Path,
        program_data: std::fs::File,
        root: LockedObject,
        sddl: &str,
        quarantined_root: Option<PathBuf>,
    ) -> io::Result<Self> {
        // Subtree first, root last, and no DACL is touched on the way down.
        // Every object ends up carrying its own protected descriptor, so a
        // parent's inheritable ACEs never decide a child's access — see
        // `harden_descendants`.
        harden_descendants(path, sddl)?;
        set_handle_security(&root.file, sddl)?;
        if !handle_security_matches(&root.file, sddl)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "machine data root did not retain its final owner/group/DACL",
            ));
        }
        let identity = root.identity;
        // The strict admission lease intentionally denies write sharing, which
        // also blocks this process's atomic child rename. Once the protected
        // DACL is exact, release that admission-only lease and reopen the same
        // identity with write sharing. An unprivileged process cannot enter the
        // gap because the final DACL is already active; identity and DACL are
        // both revalidated before the operational handle is published.
        drop(root);
        let root = open_operational_root(path)?;
        if root.identity != identity || !handle_security_matches(&root.file, sddl)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "machine data root changed during strict-to-operational handoff",
            ));
        }
        Ok(Self {
            root,
            program_data_guard: program_data,
            path: path.to_path_buf(),
            quarantined_root,
        })
    }

    /// Explicit arbitrary-parent seam for adversarial integration tests. It is
    /// absent from release builds and never consults or mutates HKLM.
    ///
    /// # Errors
    /// Returns the same validation, sharing, creation, and ACL failures as the
    /// production constructor, except registry provenance is deliberately absent.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn create_or_replace_for_test(path: &Path, sddl: &str) -> io::Result<Self> {
        Self::create_or_harden_at(path, sddl, None, false)
    }

    /// Captures the current exact root identity for a subsequent test-only
    /// provenance-verified reopen.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub const fn provenance_for_test(&self) -> TestRootProvenance {
        TestRootProvenance(self.root.identity)
    }

    /// Explicit arbitrary-parent reopen seam for adversarial integration tests.
    ///
    /// # Errors
    /// Returns provenance mismatch, validation, sharing, traversal, or ACL errors.
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    pub fn open_verified_for_test(
        path: &Path,
        provenance: TestRootProvenance,
        sddl: &str,
    ) -> io::Result<Self> {
        Self::create_or_harden_at(path, sddl, Some(provenance.0), false)
    }

    /// Returns the verified root path for APIs that only accept a path.
    ///
    /// The returned name is safe to use only while this guard remains alive:
    /// its root and `ProgramData` handles prevent replacement, and construction
    /// has removed every untrusted mutation handle from the protected tree.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the old provenance-less root name when install quarantined one.
    /// The fixed privileged name already refers to a fresh protected object;
    /// this path is informational and must never be trusted or traversed.
    #[must_use]
    pub fn quarantined_root(&self) -> Option<&Path> {
        self.quarantined_root.as_deref()
    }

    /// Reapplies owner/group/protected-DACL policy to the already pinned root.
    ///
    /// # Errors
    /// Returns SDDL conversion or `SetSecurityInfo` errors.
    pub fn set_root_security(&self, sddl: &str) -> io::Result<()> {
        set_handle_security(&self.root.file, sddl)
    }

    /// Creates one fixed direct child directory if absent and hardens its
    /// complete existing subtree on verified handles.
    ///
    /// # Errors
    /// Returns invalid-leaf, creation, type/reparse/link, sharing, or ACL errors.
    pub fn ensure_directory(&self, leaf: &str, sddl: &str) -> io::Result<()> {
        let path = self.child_path(leaf)?;
        create_directory_with_security(&path, sddl)?;
        let locked = harden_path(&path, Some(ObjectKind::Directory), sddl)?;
        drop(locked);
        Ok(())
    }

    /// Hardens one existing direct child directory and every descendant.
    ///
    /// # Errors
    /// Returns `NotFound` for an absent child or a fail-closed validation/ACL
    /// error for the verified subtree.
    pub fn harden_tree(&self, leaf: &str, sddl: &str) -> io::Result<()> {
        let path = self.child_path(leaf)?;
        let locked = harden_path(&path, Some(ObjectKind::Directory), sddl)?;
        drop(locked);
        Ok(())
    }

    /// Hardens one direct child file when it exists.
    ///
    /// # Errors
    /// Returns validation/sharing/ACL errors. A missing file is a successful
    /// no-op.
    pub fn harden_file_if_exists(&self, leaf: &str, sddl: &str) -> io::Result<()> {
        let path = self.child_path(leaf)?;
        match harden_path(&path, Some(ObjectKind::File), sddl) {
            Ok(locked) => {
                drop(locked);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Opens one direct child file for an exact, locked read.
    ///
    /// The returned `File` is the same handle whose type, reparse state, and
    /// link count were verified; callers never reopen the path.
    ///
    /// # Errors
    /// Returns open, sharing, type/reparse, or hard-link errors.
    pub fn open_file_read(&self, leaf: &str) -> io::Result<std::fs::File> {
        let path = self.child_path(leaf)?;
        Ok(open_checked(
            &path,
            Some(ObjectKind::File),
            GENERIC_READ_ACCESS | FILE_READ_ATTRIBUTES_ACCESS,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
        )?
        .file)
    }

    /// Reports whether an existing direct child file is the same NTFS object
    /// as `other` (volume serial + file ID).
    ///
    /// # Errors
    /// Returns validation or open errors other than a missing child.
    pub fn child_is_same_file(&self, leaf: &str, other: &TrustedSourceFile) -> io::Result<bool> {
        let child_path = self.child_path(leaf)?;
        let child = match open_checked(
            &child_path,
            Some(ObjectKind::File),
            FILE_READ_ATTRIBUTES_ACCESS,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
        ) {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(child.identity == other.0.identity)
    }

    /// Atomically publishes `bytes` as one direct child using a new,
    /// protected staging-file handle and a handle-relative rename.
    ///
    /// # Errors
    /// Returns leaf/target validation, create/write/sync/ACL/rename, or exact
    /// staging-handle cleanup errors.
    pub fn atomic_write(&self, leaf: &str, bytes: &[u8], sddl: &str) -> io::Result<()> {
        self.atomic_replace_with(leaf, sddl, |file| file.write_all(bytes))
    }

    /// Atomically copies `source` into one direct child using a new,
    /// protected staging-file handle and a handle-relative rename.
    ///
    /// # Errors
    /// Returns source/target validation, I/O, ACL, rename, or cleanup errors.
    pub fn atomic_copy(
        &self,
        leaf: &str,
        source: &TrustedSourceFile,
        sddl: &str,
    ) -> io::Result<()> {
        self.atomic_replace_with(leaf, sddl, |destination| {
            // `try_clone` duplicates this exact file object; it never resolves
            // the source path again after trust validation.
            let mut source = source.0.file.try_clone()?;
            source.rewind()?;
            io::copy(&mut source, destination)?;
            Ok(())
        })
    }

    fn atomic_replace_with(
        &self,
        leaf: &str,
        sddl: &str,
        write: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        let target = self.child_path(leaf)?;
        validate_replace_target(&target)?;
        let (staging_leaf, mut staging) = self.create_staging_file(sddl)?;
        let result = (|| {
            write(&mut staging)?;
            staging.flush()?;
            staging.sync_all()?;
            rename_handle_relative(&staging, &self.root.file, leaf)?;
            Ok(())
        })();
        if let Err(primary) = result {
            return match delete_handle(&staging) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(io::Error::new(
                    primary.kind(),
                    format!(
                        "{primary}; exact staging-handle cleanup for {staging_leaf} also failed: {cleanup}"
                    ),
                )),
            };
        }
        // The staging handle was deliberately opened with no sharing. After
        // rename it names the published target, so release it before reporting
        // success; otherwise an immediate reader would correctly receive
        // ERROR_SHARING_VIOLATION while this function's scope is still alive.
        drop(staging);
        Ok(())
    }

    fn create_staging_file(&self, sddl: &str) -> io::Result<(String, std::fs::File)> {
        for _ in 0..32 {
            let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
            let leaf = format!(".fmf-stage-{}-{nonce}", std::process::id());
            let path = self.path.join(&leaf);
            match create_new_file(&path, sddl) {
                Ok(file) => return Ok((leaf, file)),
                Err(error)
                    if error.raw_os_error().is_some_and(|code| {
                        [ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS].contains(&(code as u32))
                    }) => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique protected staging file",
        ))
    }

    /// Deletes one direct child file through the exact verified handle.
    ///
    /// # Errors
    /// Returns validation, sharing, or `SetFileInformationByHandle` errors. A
    /// missing file is a successful no-op.
    pub fn remove_file_if_exists(&self, leaf: &str) -> io::Result<()> {
        let path = self.child_path(leaf)?;
        match delete_path(&path, Some(ObjectKind::File)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Recursively deletes one direct child directory without following any
    /// reparse point; every removal is bound to its verified handle.
    ///
    /// # Errors
    /// Returns validation, enumeration, sharing, or exact-handle delete errors.
    /// A missing directory is a successful no-op.
    pub fn remove_tree_if_exists(&self, leaf: &str) -> io::Result<()> {
        let path = self.child_path(leaf)?;
        match delete_path(&path, Some(ObjectKind::Directory)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Deletes every descendant and finally marks the verified machine root
    /// itself for deletion on handle close.
    ///
    /// # Errors
    /// Returns the first fail-closed traversal or exact-handle delete error.
    pub fn purge(self) -> io::Result<()> {
        delete_descendants(&self.path)?;
        delete_handle(&self.root.file)?;
        let Self {
            root,
            program_data_guard,
            path: _,
            quarantined_root: _,
        } = self;
        drop(root);
        drop(program_data_guard);
        Ok(())
    }

    fn child_path(&self, leaf: &str) -> io::Result<PathBuf> {
        validate_leaf(leaf)?;
        Ok(self.path.join(leaf))
    }
}

fn validate_machine_root_path(path: &Path) -> io::Result<(&Path, &std::ffi::OsStr)> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "machine data root must be an absolute normalized path",
        ));
    }
    let leaf = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "machine data root has no leaf")
    })?;
    if !leaf.to_string_lossy().eq_ignore_ascii_case("find-my-files") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "machine data root leaf must be find-my-files",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "machine data root has no ProgramData parent",
        )
    })?;
    Ok((parent, leaf))
}

fn open_root(path: &Path) -> io::Result<LockedObject> {
    // FILE_SHARE_READ permits scanners/diagnostics but deliberately excludes
    // write and delete sharing. Security-only handles do not participate in
    // Win32 share checks; protected provenance, rather than an in-place DACL
    // rewrite, is what makes those already-granted handles harmless.
    open_checked(
        path,
        Some(ObjectKind::Directory),
        SECURITY_WRITE_ACCESS
            | DELETE_ACCESS
            | FILE_LIST_DIRECTORY_ACCESS
            | FILE_ADD_FILE_ACCESS
            | FILE_ADD_SUBDIRECTORY_ACCESS
            | FILE_TRAVERSE_ACCESS,
        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
    )
}

fn open_operational_root(path: &Path) -> io::Result<LockedObject> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    open_checked(
        path,
        Some(ObjectKind::Directory),
        SECURITY_WRITE_ACCESS
            | DELETE_ACCESS
            | FILE_LIST_DIRECTORY_ACCESS
            | FILE_ADD_FILE_ACCESS
            | FILE_ADD_SUBDIRECTORY_ACCESS
            | FILE_TRAVERSE_ACCESS,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )
}

fn quarantine_untrusted_root(
    root: std::fs::File,
    program_data: &std::fs::File,
) -> io::Result<String> {
    for _ in 0..32 {
        let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let leaf = format!(".find-my-files.untrusted-{}-{nonce}", std::process::id());
        match rename_handle_relative_with_replace(&root, program_data, &leaf, false) {
            Ok(()) => return Ok(leaf),
            Err(error)
                if error.raw_os_error().is_some_and(|code| {
                    [ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS].contains(&(code as u32))
                }) => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique quarantine name for an untrusted machine root",
    ))
}

fn create_root_from_staging(
    path: &Path,
    sddl: &str,
    program_data: &std::fs::File,
) -> io::Result<LockedObject> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "machine data root has no ProgramData parent",
        )
    })?;
    let target_leaf = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "machine data root leaf is not valid Unicode",
            )
        })?;

    for _ in 0..32 {
        let nonce = STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let staging_leaf = format!(".find-my-files.new-{}-{nonce}", std::process::id());
        let staging_path = parent.join(&staging_leaf);
        match create_directory_new_with_security(&staging_path, sddl) {
            Ok(()) => {}
            Err(error)
                if error.raw_os_error().is_some_and(|code| {
                    [ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS].contains(&(code as u32))
                }) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }

        // The directory was protected at creation. Pin that exact object before
        // giving it the privileged fixed name, then publish it relative to the
        // already pinned ProgramData handle without replacement.
        let root = open_root(&staging_path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "open protected root staging directory {}: {error}",
                    staging_path.display()
                ),
            )
        })?;
        if !handle_security_matches(&root.file, sddl)? {
            let primary = io::Error::new(
                io::ErrorKind::PermissionDenied,
                "new machine data root did not retain its creation security descriptor",
            );
            return match delete_handle(&root.file) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(io::Error::new(
                    primary.kind(),
                    format!("{primary}; exact staging-root cleanup also failed: {cleanup}"),
                )),
            };
        }
        if let Err(primary) =
            rename_handle_relative_with_replace(&root.file, program_data, target_leaf, false)
        {
            return match delete_handle(&root.file) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(io::Error::new(
                    primary.kind(),
                    format!("{primary}; exact staging-root cleanup also failed: {cleanup}"),
                )),
            };
        }
        return Ok(root);
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique protected machine-root staging directory",
    ))
}

fn open_untrusted_object_for_quarantine(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        OPEN_EXISTING,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is NUL-terminated and lives through the call; all optional
    // pointers are null, and the returned handle is checked before ownership.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            DELETE_ACCESS | FILE_READ_ATTRIBUTES_ACCESS,
            FILE_SHARE_READ,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    // SAFETY: a successful CreateFileW returns one owned kernel handle, which is
    // transferred exactly once to `File` for CloseHandle-on-drop.
    Ok(unsafe { std::fs::File::from_raw_handle(handle.cast()) })
}

struct OwnedRegistryKey(windows_sys::Win32::System::Registry::HKEY);

impl Drop for OwnedRegistryKey {
    fn drop(&mut self) {
        // SAFETY: this wrapper is constructed only from a successful registry
        // open/create and owns that non-null HKEY exactly once.
        unsafe {
            windows_sys::Win32::System::Registry::RegCloseKey(self.0);
        }
    }
}

fn provenance_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

const fn provenance_key_sddl() -> &'static str {
    "O:BAG:BAD:P(A;;KA;;;SY)(A;;KA;;;BA)"
}

fn security_descriptor_sddl(
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
) -> io::Result<String> {
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };

    let information =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    security_descriptor_sddl_for(descriptor, information)
}

fn security_descriptor_sddl_for(
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    information: windows_sys::Win32::Security::OBJECT_SECURITY_INFORMATION,
) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
    };

    let mut text: *mut u16 = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: callers pass a live self-relative descriptor; output pointers
    // refer to initialized locals and the API allocates `text` with LocalAlloc.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            information,
            &raw mut text,
            &raw mut len,
        )
    } == 0
    {
        return Err(last_error());
    }
    if text.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "security descriptor conversion returned null",
        ));
    }
    // SAFETY: success guarantees `text` addresses at least `len` UTF-16 code
    // units and the allocation remains live until LocalFree below.
    let result =
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len as usize) });
    // SAFETY: `text` is the still-owned LocalAlloc result from the successful
    // conversion call above.
    unsafe { LocalFree(text.cast()) };
    Ok(result)
}

fn handle_security_sddl(file: &std::fs::File) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    };

    let information =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `file` owns a live handle with READ_CONTROL; unused component
    // outputs are null and `descriptor` is a valid out-pointer.
    let status = unsafe {
        GetSecurityInfo(
            raw_handle(file),
            SE_FILE_OBJECT,
            information,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if descriptor.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "GetSecurityInfo returned a null security descriptor",
        ));
    }
    let result = security_descriptor_sddl(descriptor);
    // SAFETY: GetSecurityInfo allocated this non-null descriptor with LocalAlloc
    // and ownership has not escaped this function.
    unsafe { LocalFree(descriptor.cast()) };
    result
}

fn handle_security_matches(file: &std::fs::File, sddl: &str) -> io::Result<bool> {
    let actual = handle_security_sddl(file)?;
    let expected_descriptor = PipeSecurity::from_sddl(sddl)?;
    let expected = security_descriptor_sddl(expected_descriptor.descriptor)?;
    Ok(normalize_auto_inherited_control(&actual) == normalize_auto_inherited_control(&expected))
}

fn normalize_auto_inherited_control(sddl: &str) -> std::borrow::Cow<'_, str> {
    // SetSecurityInfo marks a directory DACL `AI` after it has propagated the
    // explicitly supplied OI/CI ACEs. CreateDirectoryW does not, even when both
    // calls receive the same descriptor. `AI` records that completed operation;
    // it neither changes an ACE nor permits future inheritance while `P` is set.
    //
    // Normalize only that one documented, canonical control spelling. Owner,
    // group, protected state, every ACE, and every other control bit still have
    // to match byte-for-byte in the canonical SDDL emitted by Windows.
    if sddl.contains("D:PAI(") {
        std::borrow::Cow::Owned(sddl.replacen("D:PAI(", "D:P(", 1))
    } else {
        std::borrow::Cow::Borrowed(sddl)
    }
}

fn verify_provenance_key_security(
    key: windows_sys::Win32::System::Registry::HKEY,
) -> io::Result<()> {
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::System::Registry::RegGetKeySecurity;

    let information =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut bytes = 0u32;
    // SAFETY: `key` is a live HKEY opened with READ_CONTROL; a null buffer with
    // zero capacity is the documented size-query form.
    let status =
        unsafe { RegGetKeySecurity(key, information, std::ptr::null_mut(), &raw mut bytes) };
    if status != ERROR_INSUFFICIENT_BUFFER {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "provenance-key security query returned an empty required size",
        ));
    }
    let word_bytes = size_of::<usize>();
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(word_bytes)];
    // SAFETY: the usize-backed buffer is suitably aligned and at least `bytes`
    // bytes long; the HKEY and out-size pointer remain valid through the call.
    let status =
        unsafe { RegGetKeySecurity(key, information, buffer.as_mut_ptr().cast(), &raw mut bytes) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let actual = security_descriptor_sddl(buffer.as_mut_ptr().cast())?;
    let expected_descriptor = PipeSecurity::from_sddl(provenance_key_sddl())?;
    let expected = security_descriptor_sddl(expected_descriptor.descriptor)?;
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "protected data-root provenance key has an unexpected owner/group/DACL",
        ));
    }
    Ok(())
}

fn read_root_provenance() -> io::Result<Option<ObjectIdentity>> {
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_64KEY, REG_BINARY, RegOpenKeyExW,
        RegQueryValueExW,
    };

    const ENCODED_LEN: usize = 16;

    let key_name = provenance_wide(PROVENANCE_KEY);
    let value_name = provenance_wide(PROVENANCE_VALUE);
    let mut raw_key = std::ptr::null_mut();
    // SAFETY: `key_name` is NUL-terminated; `raw_key` is a valid out-pointer and
    // the predefined HKLM handle is borrowed, not closed by us.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            key_name.as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_WOW64_64KEY | READ_CONTROL_ACCESS,
            &raw mut raw_key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if raw_key.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RegOpenKeyExW succeeded with a null key handle",
        ));
    }
    let key = OwnedRegistryKey(raw_key);
    verify_provenance_key_security(key.0)?;
    let mut value_type = 0;
    let mut size = 0u32;
    // SAFETY: `key` is live, `value_name` is NUL-terminated, and null data with a
    // valid size pointer is the documented value-size query.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            value_name.as_ptr(),
            std::ptr::null(),
            &raw mut value_type,
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if value_type != REG_BINARY || size as usize != ENCODED_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "machine data-root provenance has an invalid registry type or length",
        ));
    }
    let mut encoded = [0u8; ENCODED_LEN];
    // SAFETY: `encoded` is exactly the previously validated size and all out
    // pointers remain valid; RegQueryValueExW respects the supplied capacity.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            value_name.as_ptr(),
            std::ptr::null(),
            &raw mut value_type,
            encoded.as_mut_ptr(),
            &raw mut size,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if value_type != REG_BINARY || size as usize != ENCODED_LEN || &encoded[..4] != PROVENANCE_MAGIC
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "machine data-root provenance changed while being read",
        ));
    }
    let mut volume_serial = [0u8; 4];
    volume_serial.copy_from_slice(&encoded[4..8]);
    let mut file_id = [0u8; 8];
    file_id.copy_from_slice(&encoded[8..16]);
    Ok(Some(ObjectIdentity {
        volume_serial: u32::from_le_bytes(volume_serial),
        file_id: u64::from_le_bytes(file_id),
    }))
}

fn write_root_provenance(identity: ObjectIdentity) -> io::Result<()> {
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_BINARY,
        REG_CREATED_NEW_KEY, REG_OPENED_EXISTING_KEY, REG_OPTION_NON_VOLATILE, RegCreateKeyExW,
        RegSetValueExW,
    };

    let descriptor = PipeSecurity::from_sddl(provenance_key_sddl())?;
    let attributes = descriptor.attributes();
    let key_name = provenance_wide(PROVENANCE_KEY);
    let value_name = provenance_wide(PROVENANCE_VALUE);
    let mut raw_key = std::ptr::null_mut();
    let mut disposition = 0;
    // SAFETY: all strings are NUL-terminated, the security descriptor outlives
    // the call, and both registry outputs point to initialized storage.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            key_name.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_WOW64_64KEY | READ_CONTROL_ACCESS,
            &raw const attributes,
            &raw mut raw_key,
            &raw mut disposition,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    if raw_key.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RegCreateKeyExW succeeded with a null key handle",
        ));
    }
    let key = OwnedRegistryKey(raw_key);
    if ![REG_CREATED_NEW_KEY, REG_OPENED_EXISTING_KEY].contains(&disposition) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RegCreateKeyExW returned an unknown provenance-key disposition",
        ));
    }
    // Never "repair" an existing registry boundary in place. Like a filesystem
    // DACL, that would not revoke already-open security handles. Exact owner,
    // group, protected-DACL and ACE equality is required before trusting it.
    verify_provenance_key_security(key.0)?;

    let mut encoded = [0u8; 16];
    encoded[..4].copy_from_slice(PROVENANCE_MAGIC);
    encoded[4..8].copy_from_slice(&identity.volume_serial.to_le_bytes());
    encoded[8..16].copy_from_slice(&identity.file_id.to_le_bytes());
    // SAFETY: the HKEY is live, the value name is NUL-terminated, and `encoded`
    // remains a readable 16-byte buffer for the duration of the call.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            value_name.as_ptr(),
            0,
            REG_BINARY,
            encoded.as_ptr(),
            encoded.len() as u32,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

fn validate_leaf(leaf: &str) -> io::Result<()> {
    let mut components = Path::new(leaf).components();
    let valid = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !leaf.contains(':');
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing non-leaf machine-data name {leaf:?}"),
        ))
    }
}

/// The UTF-16 counterpart of [`validate_leaf`], for names that arrive from a
/// directory enumeration rather than from configuration.
///
/// Deliberately not "convert to `String` and reuse `validate_leaf`": a lossy
/// conversion turns an unpaired surrogate into U+FFFD, so the name that got
/// validated would not be the name that gets opened. This inspects the exact
/// code units that will be handed to the object manager and returns their byte
/// length for `UNICODE_STRING`.
///
/// Defense in depth only. A name yielded by the kernel's own enumeration of a
/// verified directory handle is already a leaf; this refuses to be the layer
/// that assumes it.
fn validate_leaf_utf16(leaf: &[u16]) -> io::Result<u16> {
    const DOT: u16 = b'.' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    const SLASH: u16 = b'/' as u16;

    let refuse = |reason: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing non-leaf directory entry name ({reason})"),
        )
    };

    if leaf.is_empty() {
        return Err(refuse("empty"));
    }
    if leaf == [DOT] || leaf == [DOT, DOT] {
        return Err(refuse("relative"));
    }
    if leaf.contains(&BACKSLASH) || leaf.contains(&SLASH) {
        return Err(refuse("path separator"));
    }
    if leaf.contains(&0) {
        return Err(refuse("embedded NUL"));
    }
    // `UNICODE_STRING::Length` is a byte count in a `u16`; leave room for the
    // terminator the object manager does not need but callers may assume.
    leaf.len()
        .checked_mul(size_of::<u16>())
        .filter(|bytes| *bytes <= usize::from(u16::MAX) - 1)
        .and_then(|bytes| u16::try_from(bytes).ok())
        .ok_or_else(|| refuse("name too long"))
}

/// Opens `leaf` **relative to an already-verified parent handle** and subjects
/// it to exactly the checks [`open_checked`] applies.
///
/// This is the primitive the managed-tree walks are built on. `open_checked`
/// hands `CreateFileW` a full path, so the kernel re-resolves every ancestor
/// name; between the moment a walk learned a child's name and the moment it
/// opens it, an ancestor can be repointed at another object. The reparse-point
/// rejection catches a link, but a swap to a *non*-reparse object at the same
/// name is invisible to it — and the walk would then apply the protected
/// descriptor to, or delete, whatever now sits there.
///
/// `NtCreateFile` with `OBJECT_ATTRIBUTES.RootDirectory` set to the parent
/// handle and a leaf-only `ObjectName` resolves nothing but the final
/// component, against the directory *object* that handle is bound to rather
/// than against its name. There is no ancestor chain left to swap.
///
/// `OBJ_DONT_REPARSE` augments — it does not replace — the existing defense:
/// `FILE_OPEN_REPARSE_POINT` is still requested and [`inspect_handle`] remains
/// the authority that refuses a reparse point, so the guarantee does not become
/// contingent on a flag whose failure mode is a different error code.
///
/// `diagnostic` is used for error messages only and is never resolved.
fn open_checked_at(
    parent: &std::fs::File,
    leaf: &[u16],
    diagnostic: &Path,
    expected: Option<ObjectKind>,
    access: u32,
    share: u32,
) -> io::Result<LockedObject> {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT,
        FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{
        OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError,
        STATUS_REPARSE_POINT_ENCOUNTERED, UNICODE_STRING,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let name_bytes = validate_leaf_utf16(leaf)?;
    // `UNICODE_STRING` is *counted*: the object manager reads exactly `Length`
    // bytes and never scans for a terminator, so `leaf` is passed as-is. This
    // is the exact opposite of the `FILE_RENAME_INFO` buffer below, whose name
    // is consumed past `FileNameLength` up to the first zero unit and therefore
    // must carry one. Do not "fix" one to look like the other.
    let name = UNICODE_STRING {
        Length: name_bytes,
        MaximumLength: name_bytes,
        Buffer: leaf.as_ptr().cast_mut(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: raw_handle(parent),
        ObjectName: &raw const name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut handle: HANDLE = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: `attributes` borrows `name`, which borrows `leaf`; all three
    // outlive the call and none is mutated during it. `parent` is a live owned
    // directory handle, so `RootDirectory` names an open kernel object. The two
    // out-parameters are exclusive borrows of correctly typed local storage.
    // The optional allocation-size and EA-buffer pointers are null, which the
    // contract permits exactly when the EA length is zero, as it is here.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            access | SYNCHRONIZE_ACCESS,
            &raw const attributes,
            &raw mut status_block,
            std::ptr::null(),
            0,
            share,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        if status == STATUS_REPARSE_POINT_ENCOUNTERED {
            // `OBJ_DONT_REPARSE` fired before `inspect_handle` could. Report it
            // in that layer's words so a caller — or a test — asserting on the
            // fail-closed property does not have to know which one won the
            // race to refuse.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("refusing reparse point {}", diagnostic.display()),
            ));
        }
        // SAFETY: a pure status-code translation with no pointer arguments.
        let code = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    // SAFETY: `NtCreateFile` succeeded, so `handle` is one owned kernel handle,
    // transferred exactly once to `File` for CloseHandle-on-drop.
    let file = unsafe { std::fs::File::from_raw_handle(handle.cast()) };
    inspect_handle(diagnostic, file, expected)
}

fn raw_handle(file: &std::fs::File) -> HANDLE {
    file.as_raw_handle().cast()
}

fn open_checked(
    path: &Path,
    expected: Option<ObjectKind>,
    access: u32,
    share: u32,
) -> io::Result<LockedObject> {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            access,
            share,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    let file = unsafe { std::fs::File::from_raw_handle(handle.cast()) };
    inspect_handle(path, file, expected)
}

fn inspect_handle(
    path: &Path,
    file: std::fs::File,
    expected: Option<ObjectKind>,
) -> io::Result<LockedObject> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(raw_handle(&file), &raw mut info) } == 0 {
        return Err(last_error());
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing reparse point {}", path.display()),
        ));
    }
    let kind = if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        ObjectKind::Directory
    } else {
        ObjectKind::File
    };
    if let Some(expected) = expected
        && expected != kind
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} has type {kind:?}, expected {expected:?}",
                path.display()
            ),
        ));
    }
    if kind == ObjectKind::File && info.nNumberOfLinks != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing multiply-linked file {} ({} links)",
                path.display(),
                info.nNumberOfLinks
            ),
        ));
    }
    Ok(LockedObject {
        file,
        identity: ObjectIdentity {
            volume_serial: info.dwVolumeSerialNumber,
            file_id: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        },
        kind,
    })
}

fn set_handle_security(file: &std::fs::File, sddl: &str) -> io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        GetSecurityDescriptorGroup, GetSecurityDescriptorOwner, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    };

    let descriptor = PipeSecurity::from_sddl(sddl)?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut group: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = 0;
    let mut present = 0;
    if unsafe {
        GetSecurityDescriptorOwner(descriptor.descriptor, &raw mut owner, &raw mut defaulted)
    } == 0
        || unsafe {
            GetSecurityDescriptorGroup(descriptor.descriptor, &raw mut group, &raw mut defaulted)
        } == 0
        || unsafe {
            GetSecurityDescriptorDacl(
                descriptor.descriptor,
                &raw mut present,
                &raw mut dacl,
                &raw mut defaulted,
            )
        } == 0
    {
        return Err(last_error());
    }
    if owner.is_null() || group.is_null() || present == 0 || dacl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "filesystem SDDL must contain owner, group, and a non-null DACL",
        ));
    }
    let information = OWNER_SECURITY_INFORMATION
        | GROUP_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | PROTECTED_DACL_SECURITY_INFORMATION;
    let status = unsafe {
        SetSecurityInfo(
            raw_handle(file),
            SE_FILE_OBJECT,
            information,
            owner,
            group,
            dacl,
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

fn create_directory_with_security(path: &Path, sddl: &str) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    let descriptor = PipeSecurity::from_sddl(sddl)?;
    let attributes = descriptor.attributes();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    if unsafe { CreateDirectoryW(wide.as_ptr(), &raw const attributes) } != 0 {
        return Ok(());
    }
    let error = last_error();
    if matches!(
        error.raw_os_error().map(|code| code as u32),
        Some(ERROR_ALREADY_EXISTS)
    ) {
        Ok(())
    } else {
        Err(error)
    }
}

fn create_directory_new_with_security(path: &Path, sddl: &str) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    let descriptor = PipeSecurity::from_sddl(sddl)?;
    let attributes = descriptor.attributes();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    if unsafe { CreateDirectoryW(wide.as_ptr(), &raw const attributes) } == 0 {
        return Err(last_error());
    }
    Ok(())
}

fn harden_path(path: &Path, expected: Option<ObjectKind>, sddl: &str) -> io::Result<LockedObject> {
    let locked = open_checked(
        path,
        expected,
        SECURITY_WRITE_ACCESS | FILE_LIST_DIRECTORY_ACCESS,
        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
    )?;
    if locked.kind == ObjectKind::Directory {
        harden_descendants(path, sddl)?;
    }
    // Only now may an inheritable ACE reach this level: the complete descendant
    // set has been opened without following reparse points, and every object
    // below already carries a protected DACL of its own.
    set_handle_security(&locked.file, sddl)?;
    Ok(locked)
}

/// Resumable state for enumerating a directory **through its own handle**,
/// one batch at a time.
///
/// This replaces `std::fs::read_dir`, which opens the directory again by path.
/// Enumerating from the handle the walk already holds keeps the child list
/// dependent only on the object that was verified, and — for the hardening
/// walk specifically — stops enumeration from depending on the very ACL the
/// walk is in the middle of rewriting.
///
/// The names it yields are an **untrusted hint about what to try next**. They
/// are not evidence about what an object is; the relative open and
/// [`inspect_handle`] remain the sole authority for that.
struct DirectoryEnumeration {
    /// `u64`-backed, and that is load-bearing twice over. `FILE_FULL_DIR_INFO`
    /// carries `i64` fields, so its alignment is 8 and the API requires a
    /// suitably aligned buffer; a `Vec<u8>` would guarantee only 1, making
    /// every record access a genuine unaligned access rather than merely a
    /// lint-flagged one. It is therefore also what lets the record cast
    /// satisfy the denied `cast_ptr_alignment` lint honestly, instead of by
    /// suppressing it.
    buffer: Vec<u64>,
    /// Bytes of `buffer` the kernel may have written in the current batch.
    valid: usize,
    /// Byte offset of the next unread record within those bytes.
    offset: usize,
    /// The kernel has reported `ERROR_NO_MORE_FILES`; no batch remains.
    done: bool,
}

impl DirectoryEnumeration {
    fn new() -> Self {
        const BUFFER_BYTES: usize = 64 * 1024;

        Self {
            buffer: vec![0u64; BUFFER_BYTES / size_of::<u64>()],
            valid: 0,
            offset: 0,
            done: false,
        }
    }
}

/// Yields the next child leaf name in `dir`, refilling the batch as needed and
/// skipping the `.` and `..` entries.
fn next_directory_entry(
    dir: &std::fs::File,
    state: &mut DirectoryEnumeration,
) -> io::Result<Option<Vec<u16>>> {
    use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FULL_DIR_INFO, FileFullDirectoryInfo, GetFileInformationByHandleEx,
    };

    const DOT: u16 = b'.' as u16;

    let malformed = |reason: &str| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing malformed directory enumeration record ({reason})"),
        )
    };

    loop {
        if state.offset >= state.valid {
            if state.done {
                return Ok(None);
            }
            let capacity = state
                .buffer
                .len()
                .checked_mul(size_of::<u64>())
                .and_then(|bytes| u32::try_from(bytes).ok())
                .ok_or_else(|| malformed("enumeration buffer exceeds u32"))?;
            // SAFETY: `dir` is a live owned directory handle. The buffer is an
            // exclusively borrowed, initialized allocation of exactly
            // `capacity` bytes, aligned to 8 as `FILE_FULL_DIR_INFO` requires,
            // and the kernel writes no further than the length it is given.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    raw_handle(dir),
                    FileFullDirectoryInfo,
                    state.buffer.as_mut_ptr().cast(),
                    capacity,
                )
            };
            if ok == 0 {
                let error = last_error();
                if error.raw_os_error().map(|code| code as u32) == Some(ERROR_NO_MORE_FILES) {
                    state.done = true;
                    return Ok(None);
                }
                return Err(error);
            }
            state.valid = capacity as usize;
            state.offset = 0;
        }

        // The kernel places every record on an 8-byte boundary. Checking that
        // rather than assuming it is what makes the name slice below sound: it
        // is the only step whose alignment is not otherwise self-evident.
        if !state
            .offset
            .is_multiple_of(align_of::<FILE_FULL_DIR_INFO>())
        {
            return Err(malformed("record is misaligned"));
        }
        if state
            .offset
            .checked_add(size_of::<FILE_FULL_DIR_INFO>())
            .is_none_or(|end| end > state.valid)
        {
            return Err(malformed("record header runs past the batch"));
        }

        let base = state.buffer.as_ptr().cast::<u8>();
        // SAFETY: the bounds check above proves `offset` — and a whole record
        // header after it — lies inside the single `buffer` allocation, and
        // the alignment check proves the resulting address is suitably aligned
        // for `FILE_FULL_DIR_INFO`.
        let record = unsafe { base.add(state.offset) }.cast::<FILE_FULL_DIR_INFO>();
        // SAFETY: `record` points at an in-bounds, initialized header. The
        // scalars are read with `read_unaligned`, so these reads stay sound
        // even if the alignment invariant above were ever weakened; no
        // reference into the packed kernel buffer is formed at any point.
        let (next_offset, name_length) = unsafe {
            (
                std::ptr::read_unaligned(&raw const (*record).NextEntryOffset),
                std::ptr::read_unaligned(&raw const (*record).FileNameLength),
            )
        };

        let name_bytes = usize::try_from(name_length)
            .map_err(|_| malformed("name length exceeds the address space"))?;
        if !name_bytes.is_multiple_of(size_of::<u16>()) {
            return Err(malformed(
                "name length is not a whole number of UTF-16 units",
            ));
        }
        let name_offset = state
            .offset
            .checked_add(std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName))
            .ok_or_else(|| malformed("name offset overflow"))?;
        if name_offset
            .checked_add(name_bytes)
            .is_none_or(|end| end > state.valid)
        {
            return Err(malformed("name runs past the batch"));
        }

        // `FileName` is a one-element array standing in for the variable-length
        // tail; take its address and reinterpret it at the declared length.
        // Deriving the pointer from the field rather than by casting a `u8`
        // pointer keeps the cast alignment-preserving (`[u16; 1]` to `u16`).
        //
        // SAFETY: the two checks above prove `name_bytes` starting at the
        // field lie inside `buffer`; the record alignment proves the address
        // is `u16`-aligned; the memory is initialized (the allocation is
        // zeroed and the kernel wrote over it); and the slice is copied out
        // before `state` can be touched again, so no aliasing outlives it.
        let name = unsafe {
            std::slice::from_raw_parts(
                (&raw const (*record).FileName).cast::<u16>(),
                name_bytes / size_of::<u16>(),
            )
        }
        .to_vec();

        state.offset = if next_offset == 0 {
            // Last record of this batch: force a refill on the next call.
            state.valid
        } else {
            state
                .offset
                .checked_add(
                    usize::try_from(next_offset).map_err(|_| malformed("offset overflow"))?,
                )
                .ok_or_else(|| malformed("offset overflow"))?
        };

        if name != [DOT] && name != [DOT, DOT] {
            return Ok(Some(name));
        }
    }
}

/// Applies the protected descriptor to every existing object below `path`,
/// bottom-up, on handles that were opened without following reparse points.
///
/// Nothing is written on the way down. A directory receives its descriptor only
/// after its whole subtree has been opened, type-checked, and given a protected
/// descriptor of its own, so an inheritable ACE can never reach a level that has
/// not been proven free of reparse points. The result is a tree in which no
/// object's access depends on what it inherits, which is what
/// [`data_tree_security_descriptors`] states as the invariant.
///
/// An earlier revision wrote a non-inheriting "quarantine" descriptor to each
/// directory on the way down, intending to lock attackers out for the duration
/// of the walk. Both halves of that were measured and neither held: the
/// descriptor removes the inherited ACEs of every child — plain files included —
/// so the walk denied itself access to `index/c.fmfidx`, while the junction
/// propagation the ordering existed to prevent never reaches the target at all.
/// Pinned by `hardening_walk_survives_a_production_shaped_tree` and
/// `planted_junction_target_is_never_reached_by_propagation`.
fn harden_descendants(path: &Path, sddl: &str) -> io::Result<()> {
    struct Frame {
        locked: Option<LockedObject>,
        entries: Option<std::fs::ReadDir>,
        depth: usize,
    }

    let mut object_count = 1usize;
    let mut stack = vec![Frame {
        locked: None,
        entries: Some(std::fs::read_dir(path)?),
        depth: 0,
    }];
    while !stack.is_empty() {
        let next = {
            let frame = stack.last_mut().ok_or_else(|| {
                io::Error::other("managed-tree hardening stack unexpectedly became empty")
            })?;
            match frame.entries.as_mut() {
                Some(entries) => entries.next().transpose()?,
                None => None,
            }
        };
        if let Some(entry) = next {
            object_count = object_count.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "managed tree object count overflow",
                )
            })?;
            if object_count > MAX_MANAGED_TREE_OBJECTS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "managed tree exceeds the {MAX_MANAGED_TREE_OBJECTS}-object safety limit"
                    ),
                ));
            }
            let depth = stack
                .last()
                .map_or(1, |frame| frame.depth.saturating_add(1));
            if depth > MAX_MANAGED_TREE_DEPTH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("managed tree exceeds the {MAX_MANAGED_TREE_DEPTH}-level safety limit"),
                ));
            }
            let child = entry.path();
            let locked = open_checked(
                &child,
                None,
                SECURITY_WRITE_ACCESS | FILE_LIST_DIRECTORY_ACCESS,
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
            )?;
            if locked.kind == ObjectKind::Directory {
                let entries = std::fs::read_dir(&child)?;
                stack.push(Frame {
                    locked: Some(locked),
                    entries: Some(entries),
                    depth,
                });
            } else {
                set_handle_security(&locked.file, sddl)?;
            }
            continue;
        }

        let mut frame = stack.pop().ok_or_else(|| {
            io::Error::other("managed-tree hardening stack unexpectedly became empty")
        })?;
        drop(frame.entries.take());
        if let Some(locked) = frame.locked {
            set_handle_security(&locked.file, sddl)?;
        }
    }
    Ok(())
}

fn validate_replace_target(path: &Path) -> io::Result<()> {
    match open_checked(
        path,
        Some(ObjectKind::File),
        FILE_READ_ATTRIBUTES_ACCESS,
        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
    ) {
        Ok(locked) => {
            drop(locked);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn create_new_file(path: &Path, sddl: &str) -> io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_FLAG_WRITE_THROUGH,
    };

    let descriptor = PipeSecurity::from_sddl(sddl)?;
    let attributes = descriptor.attributes();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE_ACCESS | DELETE_ACCESS | SECURITY_WRITE_ACCESS,
            0,
            &raw const attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_WRITE_THROUGH,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    let file = unsafe { std::fs::File::from_raw_handle(handle.cast()) };
    let locked = inspect_handle(path, file, Some(ObjectKind::File))?;
    set_handle_security(&locked.file, sddl)?;
    Ok(locked.file)
}

fn rename_handle_relative(
    source: &std::fs::File,
    root: &std::fs::File,
    target_leaf: &str,
) -> io::Result<()> {
    rename_handle_relative_with_replace(source, root, target_leaf, true)
}

fn rename_handle_relative_with_replace(
    source: &std::fs::File,
    root: &std::fs::File,
    target_leaf: &str,
    replace: bool,
) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, FILE_RENAME_INFO, FILE_RENAME_INFO_0, FileRenameInfo,
        GetFinalPathNameByHandleW, SetFileInformationByHandle, VOLUME_NAME_DOS,
    };
    const MAX_FINAL_PATH_UNITS: u32 = 32_768;

    validate_leaf(target_leaf)?;
    // Resolve the destination prefix from the already verified parent handle,
    // never from an attacker-controlled path. The parent handle was opened
    // without FILE_SHARE_DELETE, so its DOS name cannot be renamed or replaced
    // between this query and the rename operation.
    let required = unsafe {
        GetFinalPathNameByHandleW(
            raw_handle(root),
            std::ptr::null_mut(),
            0,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if required == 0 {
        return Err(last_error());
    }
    if required > MAX_FINAL_PATH_UNITS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "verified parent path exceeds the Windows extended-path limit",
        ));
    }
    let mut name = vec![0u16; required as usize];
    let written = unsafe {
        GetFinalPathNameByHandleW(
            raw_handle(root),
            name.as_mut_ptr(),
            required,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if written == 0 {
        return Err(last_error());
    }
    if written >= required {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "verified parent path changed while its replacement guard was held",
        ));
    }
    name.truncate(written as usize);
    if !name.ends_with(&[b'\\' as u16]) {
        name.push(b'\\' as u16);
    }
    name.extend(target_leaf.encode_utf16());
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename name is too long"))?;
    // FILE_RENAME_INFO ends in a one-element variable-length array. Reserve the
    // declared name *plus a terminating NUL*: measured behaviour is that the
    // name is consumed past `FileNameLength` up to the first zero unit, so an
    // unterminated buffer appends whatever follows it in memory to the created
    // name. That went unnoticed because the `usize`-backed allocation rounds up
    // to a word, which supplies a stray zero for some lengths and none for
    // others — the bug is name-length dependent, not absent.
    // `FileNameLength` itself stays the name without the terminator, as
    // documented.
    let buffer_bytes = std::mem::offset_of!(FILE_RENAME_INFO, FileName)
        .checked_add(name_bytes)
        .and_then(|bytes| bytes.checked_add(size_of::<u16>()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename buffer overflow"))?;
    let word_bytes = size_of::<usize>();
    let mut buffer = vec![0usize; buffer_bytes.div_ceil(word_bytes)];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: the usize-backed allocation satisfies FILE_RENAME_INFO alignment;
    // `buffer_bytes` covers the complete fixed structure plus every UTF-16 name
    // byte, and each field is written without first forming an unaligned ref.
    unsafe {
        // Write the union through its widest member. Writing `ReplaceIfExists`
        // instead copies the union's full four bytes from a value whose other
        // three are uninitialized, handing them to the kernel; `Flags` covers
        // all four, and its low byte is the BOOLEAN the filesystem reads.
        std::ptr::addr_of_mut!((*info).Anonymous).write(FILE_RENAME_INFO_0 {
            Flags: u32::from(replace),
        });
        std::ptr::addr_of_mut!((*info).RootDirectory).write(std::ptr::null_mut());
        std::ptr::addr_of_mut!((*info).FileNameLength).write(
            u32::try_from(name_bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "rename name exceeds u32")
            })?,
        );
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            name.len(),
        );
    }
    // SAFETY: source/root are live owned handles; the initialized aligned
    // buffer remains valid for exactly the checked byte count through the call.
    let ok = unsafe {
        SetFileInformationByHandle(
            raw_handle(source),
            FileRenameInfo,
            buffer.as_ptr().cast(),
            u32::try_from(buffer_bytes).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "rename buffer exceeds u32")
            })?,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

fn delete_path(path: &Path, expected: Option<ObjectKind>) -> io::Result<()> {
    let locked = open_checked(
        path,
        expected,
        DELETE_ACCESS | FILE_READ_ATTRIBUTES_ACCESS | FILE_LIST_DIRECTORY_ACCESS,
        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
    )?;
    if locked.kind == ObjectKind::Directory {
        delete_descendants(path)?;
    }
    delete_handle(&locked.file)?;
    drop(locked);
    Ok(())
}

fn delete_descendants(path: &Path) -> io::Result<()> {
    struct Frame {
        locked: Option<LockedObject>,
        entries: Option<std::fs::ReadDir>,
        depth: usize,
    }

    let mut object_count = 1usize;
    let mut stack = vec![Frame {
        locked: None,
        entries: Some(std::fs::read_dir(path)?),
        depth: 0,
    }];
    while !stack.is_empty() {
        let next = {
            let frame = stack.last_mut().ok_or_else(|| {
                io::Error::other("managed-tree deletion stack unexpectedly became empty")
            })?;
            match frame.entries.as_mut() {
                Some(entries) => entries.next().transpose()?,
                None => None,
            }
        };
        if let Some(entry) = next {
            object_count = object_count.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "managed tree object count overflow",
                )
            })?;
            if object_count > MAX_MANAGED_TREE_OBJECTS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "managed tree exceeds the {MAX_MANAGED_TREE_OBJECTS}-object deletion limit"
                    ),
                ));
            }
            let depth = stack
                .last()
                .map_or(1, |frame| frame.depth.saturating_add(1));
            if depth > MAX_MANAGED_TREE_DEPTH {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "managed tree exceeds the {MAX_MANAGED_TREE_DEPTH}-level deletion limit"
                    ),
                ));
            }
            let child = entry.path();
            let locked = open_checked(
                &child,
                None,
                DELETE_ACCESS | FILE_READ_ATTRIBUTES_ACCESS | FILE_LIST_DIRECTORY_ACCESS,
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
            )?;
            if locked.kind == ObjectKind::Directory {
                let entries = std::fs::read_dir(&child)?;
                stack.push(Frame {
                    locked: Some(locked),
                    entries: Some(entries),
                    depth,
                });
            } else {
                delete_handle(&locked.file)?;
            }
            continue;
        }

        let mut frame = stack.pop().ok_or_else(|| {
            io::Error::other("managed-tree deletion stack unexpectedly became empty")
        })?;
        drop(frame.entries.take());
        if let Some(locked) = frame.locked {
            delete_handle(&locked.file)?;
        }
    }
    Ok(())
}

fn delete_handle(file: &std::fs::File) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    if unsafe {
        SetFileInformationByHandle(
            raw_handle(file),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(last_error());
    }
    Ok(())
}

/// The current process token's user SID as a string ("S-1-5-21-…") —
/// `install` captures the installing user this way (docs/SECURITY.md threat 1).
///
/// # Errors
/// Returns the OS error if opening the process token, querying its user, or
/// stringifying the SID fails.
pub fn current_user_sid() -> io::Result<String> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) == 0 {
            return Err(last_error());
        }
        let token = OwnedToken(token);

        let mut needed = 0u32;
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut needed);
        let mut buf = vec![0u8; needed as usize];
        if GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        ) == 0
        {
            return Err(last_error());
        }
        // TOKEN_USER's first field is a PSID (pointer, 8-byte aligned), but the
        // Vec<u8> backing `buf` is only byte-aligned — forming a `&TOKEN_USER` to it
        // would be a misaligned reference (UB). Read the value out unaligned instead;
        // its `Sid` still points into `buf`, which outlives this use.
        let user = std::ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>());
        sid_to_string(user.User.Sid)
    }
}

/// Does `sid_str` name a real *user* account on this machine?
///
/// `install` uses it to vet a forwarded `--owner-sid` before trusting it onto
/// the pipe allowlist (docs/SECURITY.md threat 1/7): a SID that resolves to
/// nothing — or to a group / well-known principal (SYSTEM, BUILTIN\Users…)
/// — is refused. Malformed/unresolvable → `Ok(false)`.
///
/// # Errors
/// Genuine account-lookup API faults are `Err`; install fails closed.
pub fn validate_user_sid(sid_str: &str) -> io::Result<bool> {
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{LookupAccountSidW, PSID, SID_NAME_USE, SidTypeUser};

    // ConvertStringSidToSidW LocalAlloc's the SID — free it on every path.
    struct LocalSid(PSID);
    impl Drop for LocalSid {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0.cast()) };
        }
    }

    if !is_canonical_sid(sid_str) {
        return Ok(false);
    }
    let wide: Vec<u16> = sid_str.encode_utf16().chain([0]).collect();
    let mut psid: PSID = std::ptr::null_mut();
    if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut psid) } == 0 {
        return Ok(false); // not even a well-formed SID string
    }
    let owned = LocalSid(psid);

    // First call sizes the name/domain buffers; a SID that maps to no
    // account leaves them at zero (ERROR_NONE_MAPPED).
    let mut name_len = 0u32;
    let mut domain_len = 0u32;
    let mut use_kind: SID_NAME_USE = 0;
    let first_ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            owned.0,
            std::ptr::null_mut(),
            &raw mut name_len,
            std::ptr::null_mut(),
            &raw mut domain_len,
            &raw mut use_kind,
        )
    };
    if first_ok != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LookupAccountSidW unexpectedly succeeded without output buffers",
        ));
    }
    match classify_lookup_failure(unsafe { GetLastError() })? {
        LookupFailure::Unmapped => return Ok(false),
        LookupFailure::Resize => {}
    }
    if name_len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LookupAccountSidW requested an empty account-name buffer",
        ));
    }

    // Account metadata can change between the sizing and fill calls. Retry a
    // bounded number of times when Windows reports a larger required buffer;
    // every other OS fault is propagated unchanged.
    for _ in 0..4 {
        let mut name = vec![0u16; name_len as usize];
        let mut domain = vec![0u16; domain_len as usize];
        let domain_ptr = if domain.is_empty() {
            std::ptr::null_mut()
        } else {
            domain.as_mut_ptr()
        };
        let ok = unsafe {
            LookupAccountSidW(
                std::ptr::null(),
                owned.0,
                name.as_mut_ptr(),
                &raw mut name_len,
                domain_ptr,
                &raw mut domain_len,
                &raw mut use_kind,
            )
        };
        if ok != 0 {
            return Ok(use_kind == SidTypeUser);
        }
        match classify_lookup_failure(unsafe { GetLastError() })? {
            LookupFailure::Unmapped => return Ok(false),
            LookupFailure::Resize => {}
        }
    }
    Err(io::Error::other(
        "LookupAccountSidW buffer requirements changed repeatedly",
    ))
}

struct OwnedToken(HANDLE);

impl Drop for OwnedToken {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

unsafe fn sid_to_string(sid: windows_sys::Win32::Security::PSID) -> io::Result<String> {
    let mut out: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &raw mut out) } == 0 {
        return Err(last_error());
    }
    let mut len = 0;
    while unsafe { *out.add(len) } != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, len) });
    unsafe { LocalFree(out.cast()) };
    Ok(s)
}

/// Protected DACL for the data root: SYSTEM + Administrators only. The
/// snapshots inside hold every file name on the machine (SECURITY.md threat 7).
#[must_use]
pub fn data_dir_sddl() -> String {
    "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)".to_string()
}

/// logs/ keeps user read so the unelevated F12 "copy diagnostics" can tail
/// the rolling engine logs.
///
/// Each authorized user (the installing admin *and* a forwarded owner SID
/// under OTS elevation) gets read, so the daily user is never locked out of
/// its own diagnostics.
#[must_use]
pub fn logs_dir_sddl(user_sids: &[&str]) -> String {
    let mut s = String::from("O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    for sid in user_sids {
        s.push_str("(A;OICI;GR;;;");
        s.push_str(sid);
        s.push(')');
    }
    s
}

/// Protected Task Scheduler object descriptor for the SYSTEM-run daily GC.
///
/// The creating administrator is deliberately not the owner: otherwise the
/// same person's filtered unelevated token could retain implicit owner control
/// over a task that executes as SYSTEM. The descriptor is embedded in the task
/// XML so it applies atomically at registration, not in a racy post-create fix.
#[must_use]
pub const fn gc_task_sddl() -> &'static str {
    "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)"
}

/// Verifies the exact Task Scheduler file created for the daily SYSTEM GC.
///
/// The path is resolved internally from the trusted System directory and the
/// fixed task name; callers cannot redirect this privileged check.
///
/// # Errors
/// Returns Known Folder, type/reparse/link, sharing, ACL-read, or exact
/// owner/group/DACL mismatch errors.
pub fn verify_gc_task_security() -> io::Result<()> {
    let path = crate::config::system_dir()?
        .join("Tasks")
        .join(crate::lifecycle::GC_TASK_NAME);
    let locked = open_checked(
        &path,
        Some(ObjectKind::File),
        READ_CONTROL_ACCESS | FILE_READ_ATTRIBUTES_ACCESS,
        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
    )?;
    let actual = handle_security_sddl(&locked.file)?;
    if !gc_task_security_is_acceptable(&actual) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "GC task security admits a principal other than SYSTEM/Administrators, \
                 or is not protected: {:?}",
                actual.trim_end_matches('\0')
            ),
        ));
    }
    Ok(())
}

/// Whether the registered GC task's descriptor still admits nobody but SYSTEM
/// and Administrators.
///
/// Deliberately a property, not an equality. The task file's descriptor is
/// written by the Task Scheduler, not by this process: the registration XML
/// asks for one and the registrar is free to add ACEs of its own — an `SY` read
/// grant has been observed on a real install and *not* on an otherwise identical
/// re-registration. Comparing against a fixed literal therefore fails on an
/// accident of the registrar rather than on any weakening, which is what broke
/// `install` outright.
///
/// What actually matters is unchanged and is what this checks: Administrators
/// own the object, the DACL is protected so nothing is inherited into it, and
/// every ACE is an allow grant to SYSTEM or Administrators. A standard user
/// gaining any access to a task that runs as SYSTEM is the escalation; an extra
/// SYSTEM ACE is not.
fn gc_task_security_is_acceptable(sddl: &str) -> bool {
    const ADMINISTRATORS: [&str; 2] = ["BA", "S-1-5-32-544"];
    const SYSTEM: [&str; 2] = ["SY", "S-1-5-18"];

    let sddl = sddl.trim_end_matches('\0');
    let Some(rest) = sddl.strip_prefix("O:") else {
        return false;
    };
    let Some((owner, rest)) = rest.split_once("G:") else {
        return false;
    };
    let Some((group, dacl)) = rest.split_once("D:") else {
        return false;
    };
    if !ADMINISTRATORS.contains(&owner) || !ADMINISTRATORS.contains(&group) {
        return false;
    }
    // A SACL, if Windows ever emitted one here, would follow the DACL; this
    // function is only ever handed OWNER|GROUP|DACL, so its presence means the
    // string is not the shape assumed and the check must fail closed.
    let Some((flags, aces)) = dacl.split_once('(') else {
        return false;
    };
    if !flags.contains('P') || flags.contains("S:") {
        return false;
    }
    let mut remaining = format!("({aces}");
    while !remaining.is_empty() {
        let Some(stripped) = remaining.strip_prefix('(') else {
            return false;
        };
        let Some((ace, tail)) = stripped.split_once(')') else {
            return false;
        };
        let fields: Vec<&str> = ace.split(';').collect();
        let [ace_type, _flags, _rights, _object, _inherited, trustee] = fields[..] else {
            return false;
        };
        if ace_type != "A" || !(SYSTEM.contains(&trustee) || ADMINISTRATORS.contains(&trustee)) {
            return false;
        }
        remaining = tail.to_string();
    }
    true
}

/// The owner/group/DACL descriptors `install` applies across the data tree.
///
/// Returned as `(subdir, sddl)` pairs (`""` = the data root). Centralized here so
/// the threat 7 invariant — `index/` (machine-wide file-name snapshots) is
/// SYSTEM+Administrators only, never world-readable — is unit-pinned next to the
/// SDDL builders, without needing an elevated install to verify it. `install`
/// applies `index/` explicitly so the invariant does not depend on inheritance
/// history or propagation behavior.
#[must_use]
pub fn data_tree_security_descriptors(log_readers: &[&str]) -> Vec<(&'static str, String)> {
    vec![
        ("", data_dir_sddl()),
        ("index", data_dir_sddl()),
        ("logs", logs_dir_sddl(log_readers)),
    ]
}

/// The service-object DACL (ADR-0027): SYSTEM/Admins full, each user start/stop.
///
/// `O:BAG:BAD:P(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;SY)(…;;;BA)(A;;CCLCSWRPWPLORC;;;<sid>)…`
/// — Administrators owns the object (so a split-token installer is not an
/// implicit unelevated owner), SYSTEM and Administrators keep full control, and
/// each authorized user gets query-config/status + start + stop + interrogate +
/// read, nobody else.
///
/// The user ACE deliberately omits change-config (`DC`), delete (`SD`),
/// write-DAC (`WD`) and write-owner (`WO`): granting any of those to a standard
/// user on a `LocalSystem` service would let them repoint the service binary and
/// run arbitrary code as SYSTEM (local privilege escalation, docs/SECURITY.md).
/// SYSTEM keeps full control so the SYSTEM-run GC task can `DeleteService`.
#[must_use]
pub fn service_sddl(user_sids: &[String]) -> String {
    // SERVICE_ALL_ACCESS in SDDL rights letters (matches Windows' default SY/BA
    // ACEs); the per-user set is query+start+stop+interrogate+read only.
    const FULL: &str = "CCDCLCSWRPWPDTLOCRSDRCWDWO";
    let mut s = format!("O:BAG:BAD:P(A;;{FULL};;;SY)(A;;{FULL};;;BA)");
    for sid in user_sids {
        s.push_str("(A;;CCLCSWRPWPLORC;;;");
        s.push_str(sid);
        s.push(')');
    }
    s
}

/// Applies the protected service-object DACL to an exact open service handle.
///
/// The Administrators owner/group are applied at the same boundary. Callers
/// must have requested `WRITE_OWNER | WRITE_DAC` on that handle before any
/// maintenance transition. This is the only service-security mutation API, so
/// callers cannot race a name-based reopen onto a different service object.
///
/// # Errors
/// Returns SDDL conversion, descriptor-component extraction, set/query, or
/// exact read-back mismatch errors.
pub fn set_service_handle_security(
    service: &windows_service::service::Service,
    sddl: &str,
) -> io::Result<()> {
    use windows_sys::Win32::Security::Authorization::{SE_SERVICE, SetSecurityInfo};
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        GetSecurityDescriptorGroup, GetSecurityDescriptorOwner, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID,
    };

    let descriptor = PipeSecurity::from_sddl(sddl)?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut group: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: `descriptor` is a live descriptor created by the SDDL converter;
    // all outputs refer to initialized local pointer/BOOL storage.
    if unsafe {
        GetSecurityDescriptorOwner(descriptor.descriptor, &raw mut owner, &raw mut defaulted)
    } == 0
        || unsafe {
            GetSecurityDescriptorGroup(descriptor.descriptor, &raw mut group, &raw mut defaulted)
        } == 0
        || unsafe {
            GetSecurityDescriptorDacl(
                descriptor.descriptor,
                &raw mut present,
                &raw mut dacl,
                &raw mut defaulted,
            )
        } == 0
    {
        return Err(last_error());
    }
    if owner.is_null() || group.is_null() || present == 0 || dacl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "service SDDL must contain owner, group, and a non-null DACL",
        ));
    }
    // SAFETY: callers retain a live service handle with WRITE_OWNER|WRITE_DAC;
    // owner/group/DACL point inside `descriptor`, which remains live through
    // this synchronous call. The null SACL matches the selected information.
    let status = unsafe {
        SetSecurityInfo(
            service.raw_handle().cast(),
            SE_SERVICE,
            OWNER_SECURITY_INFORMATION
                | GROUP_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            group,
            dacl,
            std::ptr::null(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    verify_service_handle_security(service, sddl)?;
    Ok(())
}

fn verify_service_handle_security(
    service: &windows_service::service::Service,
    sddl: &str,
) -> io::Result<()> {
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::System::Services::QueryServiceObjectSecurity;

    let information =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut bytes = 0u32;
    // SAFETY: the retained service handle has READ_CONTROL; null/zero is the
    // documented size-query form and `bytes` is a valid out-pointer.
    if unsafe {
        QueryServiceObjectSecurity(
            service.raw_handle(),
            information,
            std::ptr::null_mut(),
            0,
            &raw mut bytes,
        )
    } != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "service security query unexpectedly succeeded without a buffer",
        ));
    }
    let error = last_error();
    if error.raw_os_error().map(|code| code as u32) != Some(ERROR_INSUFFICIENT_BUFFER) {
        return Err(error);
    }
    if bytes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "service security query returned an empty required size",
        ));
    }
    let word_bytes = size_of::<usize>();
    let mut buffer = vec![0usize; (bytes as usize).div_ceil(word_bytes)];
    // SAFETY: the usize-backed buffer is aligned for a security descriptor and
    // contains at least the byte count returned by the size query.
    if unsafe {
        QueryServiceObjectSecurity(
            service.raw_handle(),
            information,
            buffer.as_mut_ptr().cast(),
            bytes,
            &raw mut bytes,
        )
    } == 0
    {
        return Err(last_error());
    }
    let actual = security_descriptor_sddl_for(buffer.as_mut_ptr().cast(), information)?;
    let expected_descriptor = PipeSecurity::from_sddl(sddl)?;
    let expected = security_descriptor_sddl_for(expected_descriptor.descriptor, information)?;
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "service owner/group/DACL read-back did not match the requested protected descriptor",
        ));
    }
    Ok(())
}

/// Connect-time token check — defense in depth behind the DACL (a DACL
/// construction bug must not become full exposure). Empty `authorized` =
/// check disabled (console/test mode).
///
/// # Errors
/// Returns the OS error if impersonating the pipe client or reading its token
/// fails. A successfully read token that is not authorized returns `Ok(false)`.
///
/// If `RevertToSelf` fails, continuing this process could run later privileged
/// service work under the untrusted client token. Windows explicitly requires
/// fail-stop handling for that condition, so this function aborts the process;
/// SCM recovery then treats it as a service failure.
pub fn verify_client(pipe: &crate::pipe::PipeStream, authorized: &[String]) -> io::Result<bool> {
    use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    if authorized.is_empty() {
        return Ok(true);
    }
    unsafe {
        if ImpersonateNamedPipeClient(pipe.raw()) == 0 {
            return Err(last_error());
        }
    }
    // From here on we *must* revert — the closure scopes the impersonation.
    let result = (|| {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut token) == 0 {
                return Err(last_error());
            }
            let token = OwnedToken(token);
            let mut needed = 0u32;
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut needed);
            let mut buf = vec![0u8; needed as usize];
            if GetTokenInformation(
                token.0,
                TokenUser,
                buf.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            ) == 0
            {
                return Err(last_error());
            }
            // See current_user_sid: read TOKEN_USER out unaligned (its leading PSID
            // wants 8-byte alignment; the Vec<u8> is byte-aligned) so we never form a
            // misaligned reference. `buf` outlives the Sid read below.
            let user = std::ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>());
            let sid = sid_to_string(user.User.Sid)?;
            Ok(sid == "S-1-5-18" /* SYSTEM (self-connections) */
                || authorized.iter().any(|a| a == &sid))
        }
    })();
    let reverted = unsafe { windows_sys::Win32::Security::RevertToSelf() };
    if impersonation_revert_disposition(reverted != 0) == ImpersonationRevertDisposition::Abort {
        std::process::abort();
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImpersonationRevertDisposition {
    Return,
    Abort,
}

const fn impersonation_revert_disposition(reverted: bool) -> ImpersonationRevertDisposition {
    if reverted {
        ImpersonationRevertDisposition::Return
    } else {
        ImpersonationRevertDisposition::Abort
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_structure_is_pinned() {
        // Protected DACL, SYSTEM full control, per-SID read+write, nothing
        // else — the literal shape SECURITY.md documents.
        assert_eq!(pipe_sddl(&[]), "D:P(A;;GA;;;SY)");
        assert_eq!(
            pipe_sddl(&["S-1-5-21-1-2-3-1001".to_string()]),
            "D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-5-21-1-2-3-1001)"
        );
        let two = pipe_sddl(&["S-1-1-1".to_string(), "S-1-2-2".to_string()]);
        assert_eq!(two, "D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-1-1)(A;;GRGW;;;S-1-2-2)");
        assert!(!two.contains(";;;WD)"), "no Everyone ACE, ever");
        assert!(!two.contains(";;;AU)"), "no Authenticated Users ACE, ever");
        assert!(
            !two.contains(";;;BA)"),
            "no Administrators ACE (deny-only under UAC)"
        );
    }

    #[test]
    fn impersonation_reversion_failure_is_pinned_to_process_abort() {
        assert_eq!(
            impersonation_revert_disposition(true),
            ImpersonationRevertDisposition::Return
        );
        assert_eq!(
            impersonation_revert_disposition(false),
            ImpersonationRevertDisposition::Abort
        );
    }

    #[test]
    fn filesystem_readback_normalizes_only_windows_auto_inherited_history() {
        let expected = "O:BAG:BAD:P(A;OICI;FA;;;SY)\0";
        let propagated = "O:BAG:BAD:PAI(A;OICI;FA;;;SY)\0";
        assert_eq!(
            normalize_auto_inherited_control(expected),
            normalize_auto_inherited_control(propagated)
        );

        for materially_different in [
            "O:BAG:BAD:(A;OICI;FA;;;SY)\0",
            "O:BAG:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;GR;;;BU)\0",
            "O:BAG:BAD:PAI(A;OICIID;FA;;;SY)\0",
            "O:SYG:BAD:PAI(A;OICI;FA;;;SY)\0",
        ] {
            assert_ne!(
                normalize_auto_inherited_control(expected),
                normalize_auto_inherited_control(materially_different),
                "{materially_different} must remain a mismatch"
            );
        }
    }

    #[test]
    fn service_sddl_grants_user_start_stop_not_config_or_delete() {
        // SYSTEM + Administrators full control; no user → just those two ACEs.
        const FULL: &str = "CCDCLCSWRPWPDTLOCRSDRCWDWO";
        assert_eq!(
            service_sddl(&[]),
            format!("O:BAG:BAD:P(A;;{FULL};;;SY)(A;;{FULL};;;BA)")
        );
        let one = service_sddl(&["S-1-5-21-1-2-3-1001".to_string()]);
        assert_eq!(
            one,
            format!(
                "O:BAG:BAD:P(A;;{FULL};;;SY)(A;;{FULL};;;BA)(A;;CCLCSWRPWPLORC;;;S-1-5-21-1-2-3-1001)"
            )
        );

        // The user ACE grants start (RP) and stop (WP) — the whole point — but
        // NEVER change-config / delete / write-DAC / write-owner, which on a
        // LocalSystem service would be local privilege escalation.
        let user_ace = "(A;;CCLCSWRPWPLORC;;;S-1-5-21-1-2-3-1001)";
        assert!(one.contains(user_ace));
        assert!(user_ace.contains("RP"), "user can start (RP)");
        assert!(user_ace.contains("WP"), "user can stop (WP)");
        for forbidden in ["DC", "SD", "WD", "WO"] {
            assert!(
                !user_ace.contains(forbidden),
                "user ACE must not grant {forbidden} on a LocalSystem service"
            );
        }
        // And it converts to a real security descriptor.
        PipeSecurity::from_sddl(&one).expect("service SDDL converts");
        PipeSecurity::from_sddl(gc_task_sddl()).expect("GC task SDDL converts");
    }

    #[test]
    fn sddl_converts_and_user_sid_resolves() {
        // Conversion exercises the real API (unelevated-safe).
        let sec = PipeSecurity::from_sddl(&pipe_sddl(&["S-1-5-32-545".to_string()]))
            .expect("valid SDDL converts");
        assert!(!sec.attributes().lpSecurityDescriptor.is_null());

        let sid = current_user_sid().expect("own token is readable");
        assert!(sid.starts_with("S-1-"), "stringified SID: {sid}");
        // The full loop: a captured SID round-trips through the builder.
        PipeSecurity::from_sddl(&pipe_sddl(&[sid])).expect("captured SID is SDDL-legal");
    }

    #[test]
    fn validate_user_sid_accepts_self() {
        // The process's own token is a real user account.
        let sid = current_user_sid().expect("own sid");
        assert!(validate_user_sid(&sid).expect("validate own sid"));
    }

    #[test]
    fn validate_user_sid_rejects_system_and_garbage() {
        // SYSTEM resolves but is a well-known group, not a user.
        assert!(!validate_user_sid("S-1-5-18").expect("validate SYSTEM"));
        // A syntactically valid but unmapped local SID.
        assert!(
            !validate_user_sid("S-1-5-21-1111111111-2222222222-3333333333-4444")
                .expect("validate unmapped")
        );
        // Not even a SID string.
        assert!(!validate_user_sid("not-a-sid").expect("validate garbage"));
    }

    #[test]
    fn canonical_sid_parser_pins_numeric_widths_and_component_count() {
        assert!(is_canonical_sid("S-1-5"));
        assert!(is_canonical_sid(
            "S-1-5-21-1654600493-3733564142-2704359447-1001"
        ));
        for invalid in [
            "s-1-5-18",
            "S-2-5-18",
            "S-1-05-18",
            "S-1-5-21-abc",
            "S-1-281474976710656-18",
            "S-1-5-4294967296",
            "S-1-5-1-2-3-4-5-6-7-8-9-10-11-12-13-14-15-16",
        ] {
            assert!(!is_canonical_sid(invalid), "{invalid}");
        }
    }

    #[test]
    fn account_lookup_only_classifies_documented_probe_failures() {
        assert!(matches!(
            classify_lookup_failure(ERROR_INSUFFICIENT_BUFFER).expect("resize"),
            LookupFailure::Resize
        ));
        assert!(matches!(
            classify_lookup_failure(ERROR_NONE_MAPPED).expect("unmapped"),
            LookupFailure::Unmapped
        ));
        let error = classify_lookup_failure(5).expect_err("access denied is a real fault");
        assert_eq!(error.raw_os_error(), Some(5));
    }

    #[test]
    fn logs_dir_sddl_grants_read_per_user() {
        let one = logs_dir_sddl(&["S-1-5-21-1-2-3-1001"]);
        assert!(one.contains("(A;OICI;FA;;;SY)"), "SYSTEM full control");
        assert!(
            one.contains("(A;OICI;FA;;;BA)"),
            "Administrators full control"
        );
        assert!(
            one.contains("(A;OICI;GR;;;S-1-5-21-1-2-3-1001)"),
            "user read"
        );
        let two = logs_dir_sddl(&["S-1-1-1", "S-1-2-2"]);
        assert!(two.contains("(A;OICI;GR;;;S-1-1-1)"));
        assert!(two.contains("(A;OICI;GR;;;S-1-2-2)"));
    }

    #[test]
    fn data_tree_hardens_index_like_root_with_no_users() {
        let t = data_tree_security_descriptors(&["S-1-5-21-1-2-3-1001"]);
        let find = |k: &str| t.iter().find(|(s, _)| *s == k).map(|(_, v)| v.clone());
        // threat 7: index/ — machine-wide file-name snapshots — gets the SAME
        // protected SYSTEM+Admins-only DACL as the data root. Regressing this
        // (e.g. dropping the explicit index/ hardening) re-exposes every file
        // name on the machine to any local user.
        assert_eq!(find("index").as_deref(), Some(data_dir_sddl().as_str()));
        assert_eq!(find("").as_deref(), Some(data_dir_sddl().as_str()));
        assert!(
            data_dir_sddl().starts_with("O:BAG:BAD:P"),
            "Administrators own the protected tree"
        );
        PipeSecurity::from_sddl(&data_dir_sddl()).expect("data security descriptor converts");
        let index = find("index").expect("index/ must be in the hardened tree");
        for forbidden in [";;;WD)", ";;;AU)", ";;;BU)"] {
            assert!(
                !index.contains(forbidden),
                "index/ must not grant {forbidden}"
            );
        }
        // logs/ additionally grants the per-user read ACE for the F12 copy path.
        assert!(
            find("logs")
                .unwrap()
                .contains("(A;OICI;GR;;;S-1-5-21-1-2-3-1001)")
        );
    }

    #[test]
    fn trusted_root_and_child_names_are_fixed() {
        let dir = tempfile::tempdir().expect("tempdir");
        validate_machine_root_path(&dir.path().join("find-my-files")).expect("fixed machine root");
        assert_eq!(
            validate_machine_root_path(&dir.path().join("other"))
                .expect_err("wrong leaf")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            validate_machine_root_path(Path::new("find-my-files"))
                .expect_err("relative root")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        for valid in ["index", "logs", "service.json", "fmf-service.exe"] {
            validate_leaf(valid).expect(valid);
        }
        for invalid in ["", ".", "..", r"index\escape", "C:escape", "file:stream"] {
            assert_eq!(
                validate_leaf(invalid).expect_err(invalid).kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    // --- managed-tree hardening (DEV-323) ------------------------------------
    //
    // These run unelevated. The arbitrary-parent seam takes any owner, so the
    // whole walk executes under the real process SID; elevation would only
    // change the owner to Administrators. That matters, because the defect
    // these pin was invisible for exactly as long as the only tests that
    // reached the walk needed an elevated session to run at all.

    /// Fails closed. `#[ignore]` is what *skips* an elevated test; reaching the
    /// body without the arming variable means the harness was invoked outside
    /// `just test-admin`, and a silent early return would be indistinguishable
    /// from a boundary that was actually proven.
    fn require_admin_gate() {
        assert_eq!(
            std::env::var("FMF_ADMIN_TESTS").as_deref(),
            Ok("1"),
            "this ignored machine-security test must run only through `just test-admin`"
        );
    }

    fn own_tree_sddl() -> String {
        let user = current_user_sid().expect("own sid");
        format!("O:{user}G:{user}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{user})")
    }

    /// The token Windows actually *prints* for this process's user, measured
    /// by writing one ACE naming it and reading the descriptor back.
    ///
    /// A test must not assume the SID it wrote is the string it will read.
    /// `ConvertSecurityDescriptorToStringSecurityDescriptor` renders
    /// well-known accounts as two-letter SDDL aliases, and CI's Windows
    /// runners execute as `runneradmin` — the built-in Administrator, RID 500
    /// — so an ACE written with the raw `S-1-5-21-…` SID reads back as
    /// `;;;LA)` and counting the SID as a substring finds nothing. On an
    /// ordinary developer account, whose SID has no alias, the two spellings
    /// coincide and the wrong assumption is invisible.
    ///
    /// The product never depends on that round trip — `handle_security_matches`
    /// renders both sides through the same converter before comparing, so
    /// aliases cancel. Only a test counting ACEs by substring does, so it asks
    /// Windows once rather than assuming.
    fn rendered_user_token() -> &'static str {
        static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        TOKEN.get_or_init(|| {
            let user = current_user_sid().expect("own sid");
            let anchor = tempfile::tempdir().expect("probe anchor");
            let probe = anchor.path().join("sid-probe");
            // A protected DACL with exactly one ACE: nothing is inherited in,
            // so whatever sits between the last `;;;` and its `)` is the
            // rendering of `user` and nothing else.
            create_directory_new_with_security(&probe, &format!("D:P(A;;FA;;;{user})"))
                .expect("probe directory naming this process's own user");
            let sddl = read_sddl(&probe);
            let Some((token, _)) = sddl
                .rsplit_once(";;;")
                .and_then(|(_, tail)| tail.split_once(')'))
            else {
                panic!("probe descriptor carries no readable ACE: {sddl}");
            };
            assert!(!token.is_empty(), "probe ACE names nobody: {sddl}");
            token.to_string()
        })
    }

    /// Reads a descriptor with `READ_CONTROL` only. The owner keeps that right
    /// implicitly even when the DACL grants nothing, so a stripped object stays
    /// *observable* instead of merely inaccessible.
    fn read_sddl(path: &Path) -> String {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let locked = open_checked(
            path,
            None,
            READ_CONTROL_ACCESS,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
        .unwrap_or_else(|error| panic!("read security of {}: {error}", path.display()));
        handle_security_sddl(&locked.file)
            .expect("descriptor converts")
            .trim_end_matches('\0')
            .to_string()
    }

    /// Attempts exactly the open the hardening walk performs.
    fn walk_open(path: &Path) -> io::Result<LockedObject> {
        open_checked(
            path,
            None,
            SECURITY_WRITE_ACCESS | FILE_LIST_DIRECTORY_ACCESS,
            windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ,
        )
    }

    /// Plants an NTFS junction at `link` pointing at `target`.
    ///
    /// Deliberately not `symlink_dir`: a symlink needs `SeCreateSymbolicLink`
    /// or Developer Mode, while any standard user can create a junction. The
    /// unprivileged one is the case the threat model cares about, and it is the
    /// one an elevation-gated test would never have covered.
    fn create_junction(link: &Path, target: &Path) -> io::Result<()> {
        use std::os::windows::io::FromRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::IO::DeviceIoControl;
        use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;

        const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

        std::fs::create_dir(link)?;

        let substitute: Vec<u16> = format!(r"\??\{}", target.display())
            .encode_utf16()
            .collect();
        let print: Vec<u16> = target.display().to_string().encode_utf16().collect();
        let substitute_bytes = u16::try_from(substitute.len() * 2).expect("substitute name fits");
        let print_bytes = u16::try_from(print.len() * 2).expect("print name fits");

        let mut path_buffer: Vec<u16> = substitute;
        path_buffer.push(0);
        path_buffer.extend_from_slice(&print);
        path_buffer.push(0);

        let mut buffer: Vec<u8> = Vec::new();
        buffer.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        let data_length = 8 + u16::try_from(path_buffer.len() * 2).expect("reparse data fits");
        buffer.extend_from_slice(&data_length.to_le_bytes());
        buffer.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buffer.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset
        buffer.extend_from_slice(&substitute_bytes.to_le_bytes());
        buffer.extend_from_slice(&(substitute_bytes + 2).to_le_bytes()); // PrintNameOffset
        buffer.extend_from_slice(&print_bytes.to_le_bytes());
        for unit in path_buffer {
            buffer.extend_from_slice(&unit.to_le_bytes());
        }

        let wide: Vec<u16> = link.as_os_str().encode_wide().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call; the optional
        // pointers are null and the handle is checked before ownership.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE_ACCESS,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        // SAFETY: one owned kernel handle, transferred exactly once to `File`
        // for CloseHandle-on-drop.
        let file = unsafe { std::fs::File::from_raw_handle(handle.cast()) };
        let mut returned = 0u32;
        // SAFETY: `buffer` is a correctly laid out mount-point REPARSE_DATA_BUFFER
        // whose declared length matches its allocation; no output is requested.
        let ok = unsafe {
            DeviceIoControl(
                raw_handle(&file),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                u32::try_from(buffer.len()).expect("reparse buffer fits"),
                std::ptr::null_mut(),
                0,
                &raw mut returned,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// The shape every install *after the first one* walks, and the shape no
    /// previously passing test built: a child directory that holds only files.
    /// Every earlier test created an empty root, so `harden_descendants`
    /// iterated nothing and the walk was never executed at all.
    #[test]
    fn hardening_walk_survives_a_production_shaped_tree() {
        let sddl = own_tree_sddl();
        let anchor = tempfile::tempdir().expect("anchor");
        let root = anchor.path().join("find-my-files");

        let trusted =
            TrustedDataRoot::create_or_replace_for_test(&root, &sddl).expect("fresh root");
        let provenance = trusted.provenance_for_test();
        drop(trusted);

        let index = root.join("index");
        std::fs::create_dir(&index).expect("index");
        std::fs::write(index.join("c.fmfidx"), b"payload").expect("index payload");
        std::fs::write(index.join(".writer.lock"), b"").expect("writer lock");
        std::fs::create_dir(index.join("nested")).expect("nested");
        std::fs::write(index.join("nested").join("deep.bin"), b"deep").expect("deep payload");
        std::fs::write(root.join("service.json"), b"{}").expect("service.json");

        let trusted = TrustedDataRoot::open_verified_for_test(&root, provenance, &sddl)
            .expect("a populated managed tree must harden without denying the walk its own access");

        // The second site: `ensure_directory` hardens one child subtree through
        // `harden_path`, which is what the real install calls for `index/`.
        trusted
            .ensure_directory("index", &sddl)
            .expect("re-hardening an existing populated child must succeed");

        for relative in [
            Path::new("index"),
            Path::new("index/c.fmfidx"),
            Path::new("index/.writer.lock"),
            Path::new("index/nested"),
            Path::new("index/nested/deep.bin"),
            Path::new("service.json"),
        ] {
            let path = root.join(relative);
            walk_open(&path).unwrap_or_else(|error| {
                panic!(
                    "{} is unreachable after hardening: {error}",
                    relative.display()
                )
            });
            let actual = read_sddl(&path);
            assert!(
                actual.contains("D:P"),
                "{} must carry its own protected DACL, not depend on inheritance: {actual}",
                relative.display()
            );
        }

        assert_eq!(
            std::fs::read(index.join("c.fmfidx")).expect("index payload still readable"),
            b"payload"
        );
    }

    /// DEV-322 #1, measured rather than argued.
    ///
    /// A handle holding only `FILE_DELETE_CHILD` cannot be excluded by any share
    /// mode — Windows buckets share access into read/write/delete and that right
    /// is in none of them — so `create_or_replace_for_test` cannot fail closed
    /// on one, and does not. The open question was whether rotating the squatted
    /// object out of the privileged name is *sufficient*, which only a real
    /// standard-user token can answer: in the arbitrary-parent seam this process
    /// is the owner and holds full control, so any attempt it makes would
    /// succeed for the wrong reason.
    ///
    /// The answer is that the handle is bound to the object, not the name. It
    /// keeps its capability over the quarantined directory it was opened on, and
    /// has none over the fresh root, whose descriptor admits only SYSTEM and
    /// Administrators.
    #[test]
    #[ignore = "creates a real local account and impersonates it; gated by FMF_ADMIN_TESTS=1"]
    fn a_delete_child_handle_keeps_the_quarantined_object_and_never_reaches_the_new_root() {
        use std::os::windows::io::FromRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };

        use crate::pipe::admin_security_tests::{TemporaryLocalUser, with_impersonated_user};

        const FILE_DELETE_CHILD: u32 = 0x0000_0040;

        fn open_for(path: &Path, access: u32) -> io::Result<std::fs::File> {
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            // SAFETY: `wide` is NUL-terminated and outlives the call; optional
            // pointers are null and the handle is checked before ownership.
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
            if handle == INVALID_HANDLE_VALUE {
                return Err(last_error());
            }
            // SAFETY: one owned kernel handle, transferred exactly once.
            Ok(unsafe { std::fs::File::from_raw_handle(handle.cast()) })
        }

        require_admin_gate();
        let anchor = tempfile::tempdir().expect("anchor");
        // The squatter needs somewhere it can create the fixed name. Users, not
        // a looked-up SID: the ephemeral account is a member and this is a temp
        // directory that exists for the length of the test.
        set_handle_security(
            &open_for(anchor.path(), SECURITY_WRITE_ACCESS).expect("anchor handle"),
            "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;BU)",
        )
        .expect("let the ephemeral user write into the anchor");

        let mut user = TemporaryLocalUser::create();
        let token = user.logon();
        let root = anchor.path().join("find-my-files");

        // 1. The squatter creates the fixed name, puts something in it, and
        //    keeps a delete-child handle open across the whole install.
        let stale = with_impersonated_user(&token, || {
            std::fs::create_dir(&root).expect("squatted root");
            std::fs::write(root.join("loot"), b"x").expect("squatted child");
            open_for(&root, FILE_DELETE_CHILD).expect("delete-child handle")
        });

        // 2. Install proceeds — it cannot fail closed on this right — and
        //    rotates the squatted object out of the privileged name.
        let trusted = TrustedDataRoot::create_or_replace_for_test(&root, &data_dir_sddl())
            .expect("a delete-child handle cannot be excluded by share mode");
        let quarantine = trusted
            .quarantined_root()
            .expect("the squatted object must be rotated out")
            .to_path_buf();
        trusted
            .atomic_write("service.json", b"{}", &data_dir_sddl())
            .expect("publish into the fresh root");

        // 3. What the squatter can still do, and what it cannot.
        with_impersonated_user(&token, || {
            std::fs::remove_file(quarantine.join("loot"))
                .expect("the capability follows the object it was opened on");

            let denied = open_for(&root.join("service.json"), DELETE_ACCESS)
                .expect_err("the fresh root must be unreachable");
            assert_eq!(denied.raw_os_error(), Some(5), "{denied}");

            let listing =
                std::fs::read_dir(&root).expect_err("nor may it even enumerate the fresh root");
            assert_eq!(listing.raw_os_error(), Some(5), "{listing}");
        });

        drop(stale);
        drop(trusted);
        user.remove();
    }

    /// Both descriptors below were observed on this machine from the *same*
    /// registration XML — the registrar appended an `SY` read ACE on one install
    /// and not on a re-registration. That is why this is a property check.
    #[test]
    fn gc_task_security_accepts_registrar_variation_but_no_other_principal() {
        for accepted in [
            "O:BAG:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)",
            "O:BAG:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;SY)",
            "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)",
            gc_task_sddl(),
            "O:S-1-5-32-544G:S-1-5-32-544D:PAI(A;;FA;;;S-1-5-18)(A;;FA;;;S-1-5-32-544)",
            // A trailing NUL is how the readback arrives from Windows.
            "O:BAG:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)\0",
        ] {
            assert!(
                gc_task_security_is_acceptable(accepted),
                "must accept {accepted:?}"
            );
        }

        for rejected in [
            // Any other principal on a task that runs as SYSTEM is the escalation.
            "O:BAG:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;BU)",
            "O:BAG:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)",
            "O:BAG:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;S-1-5-21-1-2-3-1001)",
            // Unprotected: the descriptor could then be widened by inheritance.
            "O:BAG:BAD:AI(A;;FA;;;SY)(A;;FA;;;BA)",
            // Owned or grouped by anyone else.
            "O:BUG:BAD:PAI(A;;FA;;;SY)(A;;FA;;;BA)",
            "O:BAG:BUD:PAI(A;;FA;;;SY)(A;;FA;;;BA)",
            // Deny ACEs are not part of the shape this accepts.
            "O:BAG:BAD:PAI(D;;FA;;;SY)(A;;FA;;;BA)",
            // Malformed rather than merely different — must fail closed.
            "O:BAG:BAD:PAI(A;;FA;;;SY",
            "not-a-descriptor",
            "",
        ] {
            assert!(
                !gc_task_security_is_acceptable(rejected),
                "must reject {rejected:?}"
            );
        }
    }

    /// A handle-relative rename must produce *exactly* the requested leaf.
    ///
    /// Found on a real install: the quarantined root landed on disk as
    /// `.find-my-files.untrusted-7156-0` followed by four UTF-16 units of
    /// unrelated process memory. The caller's reported path and the object that
    /// exists then disagree, and install fails looking for a name that is not
    /// there — which is how it surfaced.
    ///
    /// Every leaf length is swept, because the defect is length-dependent: the
    /// `usize`-backed buffer rounds up to a word, so some lengths happen to
    /// leave a stray zero after the name and terminate correctly while others
    /// do not. Testing one name proves nothing about another.
    #[test]
    fn handle_relative_rename_produces_exactly_the_requested_leaf() {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let anchor = tempfile::tempdir().expect("anchor");
        let parent = open_checked(
            anchor.path(),
            Some(ObjectKind::Directory),
            FILE_READ_ATTRIBUTES_ACCESS | FILE_ADD_SUBDIRECTORY_ACCESS | FILE_TRAVERSE_ACCESS,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        )
        .expect("parent handle");

        let mut leaves: Vec<String> = (1..=40).map(|len| "a".repeat(len)).collect();
        leaves.push(".find-my-files.untrusted-7156-0".to_string());
        leaves.push(".fmf-stage-1-2".to_string());

        for leaf in leaves {
            let source_path = anchor.path().join("source");
            std::fs::create_dir(&source_path).expect("source directory");
            let source = open_checked(
                &source_path,
                Some(ObjectKind::Directory),
                DELETE_ACCESS | FILE_READ_ATTRIBUTES_ACCESS,
                FILE_SHARE_READ,
            )
            .expect("source handle");

            rename_handle_relative_with_replace(&source.file, &parent.file, &leaf, false)
                .unwrap_or_else(|error| panic!("rename to {leaf:?}: {error}"));
            drop(source);

            let found: Vec<String> = std::fs::read_dir(anchor.path())
                .expect("list anchor")
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                found,
                vec![leaf.clone()],
                "the renamed object must carry exactly the requested leaf, with nothing appended"
            );
            std::fs::remove_dir(anchor.path().join(&leaf)).expect("clean up");
        }
    }

    /// The recursion bound is the property that matters: an unbounded walk is a
    /// stack-overflow denial of service against a `LocalSystem` service.
    ///
    /// Its elevated sibling asserted this and never reached the depth check —
    /// the walk denied itself access first, and `PermissionDenied` was mistaken
    /// for the bound holding. Proving it unelevated means every PR proves it.
    #[test]
    fn depth_bound_is_what_stops_the_walk() {
        let sddl = own_tree_sddl();
        let anchor = tempfile::tempdir().expect("anchor");
        let root = anchor.path().join("find-my-files");
        let trusted =
            TrustedDataRoot::create_or_replace_for_test(&root, &sddl).expect("fresh root");
        let provenance = trusted.provenance_for_test();
        drop(trusted);

        let mut cursor = root.clone();
        for _ in 0..=MAX_MANAGED_TREE_DEPTH {
            cursor.push("d");
            std::fs::create_dir(&cursor).expect("deep directory");
        }

        let error = TrustedDataRoot::open_verified_for_test(&root, provenance, &sddl)
            .expect_err("the depth limit must fail closed");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
        assert!(
            error
                .to_string()
                .contains(&format!("{MAX_MANAGED_TREE_DEPTH}-level")),
            "the depth bound, not an access failure, must be what refuses: {error}"
        );
    }

    /// The root-level walk applies one descriptor to the whole tree, so it
    /// flattens `logs/`'s narrower policy along with everything else.
    ///
    /// That is never a widening — `data_dir_sddl` is the strictest of the set —
    /// but it does mean the re-application of `data_tree_security_descriptors`
    /// after `open_and_harden_machine` is load-bearing, not belt-and-braces:
    /// without it the unelevated F12 diagnostics read is gone. Pinned here so
    /// that coupling cannot be deleted as redundant.
    #[test]
    fn root_walk_flattens_per_path_policy_and_must_be_followed_by_reapplication() {
        let user = current_user_sid().expect("own sid");
        let root_sddl = own_tree_sddl();
        let logs_sddl = format!("{root_sddl}(A;OICI;GR;;;{user})");

        let anchor = tempfile::tempdir().expect("anchor");
        let root = anchor.path().join("find-my-files");
        let trusted =
            TrustedDataRoot::create_or_replace_for_test(&root, &root_sddl).expect("fresh root");
        let provenance = trusted.provenance_for_test();
        trusted
            .ensure_directory("logs", &logs_sddl)
            .expect("logs with its own reader ACE");
        std::fs::write(root.join("logs").join("engine.log"), b"line").expect("log file");
        trusted
            .harden_tree("logs", &logs_sddl)
            .expect("apply the logs policy to the file too");
        drop(trusted);

        // Count ACEs naming the user rather than matching `GR`: a generic right
        // is mapped to specific rights when it lands on a real object, so the
        // literal does not survive application. The *trustee* spelling does not
        // survive either when the account is well-known, hence the probed
        // token instead of the SID this test wrote (see `rendered_user_token`).
        let log_file = root.join("logs").join("engine.log");
        let printed = rendered_user_token();
        let user_aces = |path: &Path| read_sddl(path).matches(&format!(";;;{printed})")).count();
        assert_eq!(
            user_aces(&log_file),
            2,
            "precondition: the log file carries both the owner grant and the reader grant"
        );

        // A service start re-verifies the root with the *root* descriptor.
        let trusted = TrustedDataRoot::open_verified_for_test(&root, provenance, &root_sddl)
            .expect("root verification");
        assert_eq!(
            user_aces(&log_file),
            1,
            "the root-level walk is expected to flatten logs/ — if this ever stops \
             being true, the re-application below is no longer load-bearing"
        );

        // …which is exactly why the caller re-applies the per-path policy.
        trusted.harden_tree("logs", &logs_sddl).expect("re-apply");
        assert_eq!(
            user_aces(&log_file),
            2,
            "re-application must restore the unelevated diagnostics read"
        );
    }

    /// The junction-propagation threat, measured rather than assumed.
    ///
    /// The walk's ordering exists to stop an inheritable DACL from reaching a
    /// planted junction's target. This pins both halves: applying that DACL to
    /// a root containing a junction leaves the target untouched, and the walk
    /// rejects the junction instead of descending through it.
    #[test]
    fn planted_junction_target_is_never_reached_by_propagation() {
        let sddl = own_tree_sddl();
        let anchor = tempfile::tempdir().expect("anchor");
        let outside = tempfile::tempdir().expect("outside");
        let outside_file = outside.path().join("sentinel");
        std::fs::write(&outside_file, b"outside").expect("sentinel");

        let root = anchor.path().join("find-my-files");
        create_directory_new_with_security(&root, &sddl).expect("root");
        let link = root.join("index");
        create_junction(&link, outside.path()).expect("plant a junction");

        let before_dir = read_sddl(outside.path());
        let before_file = read_sddl(&outside_file);

        let root_locked = open_root(&root).expect("open_root");
        set_handle_security(&root_locked.file, &sddl).expect("apply the inheritable descriptor");
        assert_eq!(
            read_sddl(outside.path()),
            before_dir,
            "propagation must not cross a junction into its target directory"
        );
        assert_eq!(
            read_sddl(&outside_file),
            before_file,
            "propagation must not cross a junction into the target's contents"
        );
        drop(root_locked);

        let error =
            harden_descendants(&root, &sddl).expect_err("a planted junction must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("reparse point"), "{error}");
        assert_eq!(
            std::fs::read(&outside_file).expect("target contents survive"),
            b"outside"
        );

        std::fs::remove_dir(&link).expect("remove the junction, not its target");
    }
}
