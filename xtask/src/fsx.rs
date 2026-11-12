//! Filesystem helpers with the `-Force` semantics the PowerShell recipes had.
//!
//! `std::fs::remove_dir_all` fails on Windows the moment it hits a read-only file (the OS refuses to delete one), and published bundles are full of them (`ReadyToRun` DLLs, `PreserveNewest` copies).
//! This clears the read-only attribute and retries — matching `Remove-Item -Recurse -Force`.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

/// Atomically replace `path` with `bytes`, using a same-directory temporary file and a write-through replace on Windows.
/// An interrupted update therefore leaves either the complete old file or the complete new file, never a gap or partially written release artifact.
pub fn write_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    const MAX_TEMPORARY_NAMES: u8 = 64;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let file_name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output filename is not Unicode: {}", path.display()),
        )
    })?;

    let mut temporary = None;
    for suffix in 0..MAX_TEMPORARY_NAMES {
        let candidate = parent.join(format!(".{file_name}.tmp-{}-{suffix}", std::process::id()));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary output path",
        )
    })?;
    let result = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary_path, path)
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary_path) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "could not remove temporary output {}: {error}",
                    temporary_path.display()
                );
            }
        }
    }
    result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    /* SAFETY: Both paths remain live, NUL-terminated UTF-16 buffers throughout the synchronous call. */
    let moved = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// True for symbolic links and every other Windows reparse-point kind (junctions, mount points, cloud placeholders, and future variants).
#[cfg(windows)]
pub fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// Recursively copy `src` into `dst`, creating `dst` (and parents) and overwriting existing files — `Copy-Item -Recurse -Force` for a directory.
pub fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Recursively delete `path`.
/// A missing path is success (the old recipes' `-ErrorAction SilentlyContinue; exit 0`).
/// Read-only entries are forced.
pub fn force_remove_dir_all(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(first) => remove_recursive(path).map_err(|retry| {
            io::Error::new(
                retry.kind(),
                format!("delete {} failed: {first}; retry: {retry}", path.display()),
            )
        }),
    }
}

fn remove_recursive(path: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            remove_recursive(&entry?.path())?;
        }
        retry_clearing_readonly(path, &meta, |p| fs::remove_dir(p))
    } else {
        retry_clearing_readonly(path, &meta, |p| fs::remove_file(p))
    }
}

fn retry_clearing_readonly(
    path: &Path,
    meta: &fs::Metadata,
    remove: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if remove(path).is_ok() {
        return Ok(());
    }
    let mut perms = meta.permissions();
    #[cfg(windows)]
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "This Windows-only branch changes the read-only attribute; Unix preserves the other mode bits."
    )]
    perms.set_readonly(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        const OWNER_WRITE: u32 = 0o200;
        perms.set_mode(perms.mode() | OWNER_WRITE);
    }
    fs::set_permissions(path, perms)?;
    remove(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("xtask-fsx-{tag}-{}", std::process::id()))
    }

    #[test]
    fn removes_a_tree_containing_a_readonly_file() {
        let base = scratch("ro");
        let _ = force_remove_dir_all(&base);
        let nested = base.join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        let f = nested.join("readonly.txt");
        fs::write(&f, b"x").unwrap();
        let mut perms = fs::metadata(&f).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&f, perms).unwrap();

        force_remove_dir_all(&base).unwrap();
        assert!(!base.exists(), "tree should be gone");
    }

    #[cfg(unix)]
    #[test]
    fn readonly_retry_preserves_group_and_other_permission_bits() {
        use std::cell::Cell;
        use std::os::unix::fs::PermissionsExt;

        let base = scratch("permission-bits");
        fs::create_dir_all(&base).unwrap();
        let file = base.join("readonly.txt");
        fs::write(&file, b"x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o440)).unwrap();
        let metadata = fs::metadata(&file).unwrap();
        let attempts = Cell::new(0);
        retry_clearing_readonly(&file, &metadata, |path| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o640
                );
                fs::remove_file(path)
            }
        })
        .unwrap();
        assert_eq!(attempts.get(), 2);
        force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_path_is_ok() {
        assert!(force_remove_dir_all(&scratch("missing")).is_ok());
    }

    #[test]
    fn copies_a_tree_recursively() {
        let base = scratch("copy");
        let _ = force_remove_dir_all(&base);
        let src = base.join("src");
        let dst = base.join("dst");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("top.txt"), b"top").unwrap();
        fs::write(src.join("sub").join("nested.txt"), b"nested").unwrap();

        copy_dir_all(&src, &dst).unwrap();
        assert_eq!(fs::read(dst.join("top.txt")).unwrap(), b"top");
        assert_eq!(
            fs::read(dst.join("sub").join("nested.txt")).unwrap(),
            b"nested"
        );

        force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn atomic_write_replaces_a_complete_existing_file() {
        let base = scratch("atomic-write");
        let _ = force_remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let path = base.join("artifact.json");
        fs::write(&path, b"old").unwrap();

        write_file_atomic(&path, b"new-complete-body").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new-complete-body");
        let entries = fs::read_dir(&base).unwrap().count();
        assert_eq!(entries, 1, "atomic replace must not leave temp files");
        force_remove_dir_all(&base).unwrap();
    }
}
