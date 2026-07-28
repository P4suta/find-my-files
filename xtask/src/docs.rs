//! Assemble the deliberately small Pages artifact: committed landing page plus
//! the canonical mdBook. Implementation API documentation is not published.

use std::fs;
use std::path::Path;

use crate::{fsx, paths};
use anyhow::{bail, Context, Result};

fn require_nonempty(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("missing required documentation file {}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        bail!(
            "required documentation file is empty or not a file: {}",
            path.display()
        );
    }
    Ok(())
}

fn assemble(landing: &Path, book: &Path, site: &Path) -> Result<()> {
    for required in [
        landing.join("index.html"),
        landing.join("en").join("index.html"),
        landing.join("style.css"),
        book.join("index.html"),
    ] {
        require_nonempty(&required)?;
    }

    // Build a complete candidate next to the final directory. Publishing must
    // never merge into an earlier site: mdBook fingerprints assets, so a merge
    // silently retains obsolete hashes and dead pages forever.
    let candidate = site.with_extension("candidate");
    fsx::force_remove_dir_all(&candidate)
        .with_context(|| format!("clear stale candidate {}", candidate.display()))?;

    let copied = (|| -> Result<()> {
        fsx::copy_dir_all(landing, &candidate)
            .with_context(|| format!("copy {} -> {}", landing.display(), candidate.display()))?;
        fsx::copy_dir_all(book, &candidate.join("book")).with_context(|| {
            format!(
                "copy {} -> {}",
                book.display(),
                candidate.join("book").display()
            )
        })?;
        Ok(())
    })();
    if let Err(error) = copied {
        let _ = fsx::force_remove_dir_all(&candidate);
        return Err(error);
    }

    fsx::force_remove_dir_all(site)
        .with_context(|| format!("clear previous site {}", site.display()))?;
    fs::rename(&candidate, site).with_context(|| {
        format!(
            "promote complete documentation site {} -> {}",
            candidate.display(),
            site.display()
        )
    })?;
    Ok(())
}

pub fn run() -> Result<()> {
    let root = paths::repo_root();
    let site = paths::site_dir();

    let landing = root.join("site");
    if !landing.is_dir() {
        bail!("missing {} — the committed landing page", landing.display());
    }

    let book = paths::build_root().join("docs-book");
    if !book.is_dir() {
        bail!(
            "missing {} — build the docs first (just doc)",
            book.display()
        );
    }
    assemble(&landing, &book, &site)?;

    println!("docs-assemble: assembled landing + canonical book into build/site/");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("xtask-docs-{tag}-{}", std::process::id()))
    }

    fn write_required_sources(base: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let landing = base.join("landing");
        let book = base.join("book");
        fs::create_dir_all(landing.join("en")).unwrap();
        fs::create_dir_all(&book).unwrap();
        fs::write(landing.join("index.html"), b"ja").unwrap();
        fs::write(landing.join("en").join("index.html"), b"en").unwrap();
        fs::write(landing.join("style.css"), b"css").unwrap();
        fs::write(book.join("index.html"), b"book").unwrap();
        (landing, book)
    }

    #[test]
    fn assembly_replaces_the_whole_site_without_stale_assets() {
        let base = scratch("replace");
        let _ = fsx::force_remove_dir_all(&base);
        let (landing, book) = write_required_sources(&base);
        let site = base.join("site");
        fs::create_dir_all(site.join("book")).unwrap();
        fs::write(site.join("book").join("old-hash.js"), b"stale").unwrap();

        assemble(&landing, &book, &site).unwrap();

        assert_eq!(fs::read(site.join("index.html")).unwrap(), b"ja");
        assert_eq!(
            fs::read(site.join("book").join("index.html")).unwrap(),
            b"book"
        );
        assert!(!site.join("book").join("old-hash.js").exists());
        assert!(!site.with_extension("candidate").exists());
        fsx::force_remove_dir_all(&base).unwrap();
    }

    #[test]
    fn missing_input_leaves_the_previous_site_untouched() {
        let base = scratch("invalid");
        let _ = fsx::force_remove_dir_all(&base);
        let (landing, book) = write_required_sources(&base);
        fs::remove_file(landing.join("style.css")).unwrap();
        let site = base.join("site");
        fs::create_dir_all(&site).unwrap();
        fs::write(site.join("keep.txt"), b"old-good").unwrap();

        assert!(assemble(&landing, &book, &site).is_err());
        assert_eq!(fs::read(site.join("keep.txt")).unwrap(), b"old-good");
        assert!(!site.with_extension("candidate").exists());
        fsx::force_remove_dir_all(&base).unwrap();
    }
}
