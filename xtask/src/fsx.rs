//! Filesystem helpers with the `-Force` semantics the PowerShell recipes had.
//!
//! `std::fs::remove_dir_all` fails on Windows the moment it hits a read-only
//! file (the OS refuses to delete one), and published bundles are full of them
//! (`ReadyToRun` DLLs, `PreserveNewest` copies). This clears the read-only
//! attribute and retries — matching `Remove-Item -Recurse -Force`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Collect every file under `root`, recursively, as
/// `(absolute path, path relative to root using forward slashes)`. The relative
/// form is the entry name for a zip; empty directories are skipped (a runnable
/// bundle has none that matter).
pub fn collect_files(root: &Path) -> io::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .expect("walked path is under root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((path, rel));
        }
    }
    Ok(())
}

/// Recursively copy `src` into `dst`, creating `dst` (and parents) and
/// overwriting existing files — `Copy-Item -Recurse -Force` for a directory.
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

/// Recursively delete `path`. A missing path is success (the old recipes'
/// `-ErrorAction SilentlyContinue; exit 0`). Read-only entries are forced.
pub fn force_remove_dir_all(path: &Path) -> io::Result<()> {
    // Fast path: the common case (nothing read-only) needs no extra syscalls.
    match fs::remove_dir_all(path) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {} // fall through to the read-only-clearing slow path
    }
    remove_recursive(path)
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
    // Clearing read-only is the only way to delete a read-only file on Windows
    // (the failure that brought us here). The entry is deleted on the very next
    // line, so the brief Unix "world-writable" window the lint warns about is on
    // a doomed file — harmless. This slow path is essentially Windows-only
    // anyway (Unix deletes by parent-dir permission, so the fast path wins).
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut perms = meta.permissions();
        perms.set_readonly(false);
        fs::set_permissions(path, perms)?;
    }
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
        let _ = force_remove_dir_all(&base); // clean any leftover
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
}
