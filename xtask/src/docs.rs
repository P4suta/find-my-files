//! `xtask docs-assemble` — stage the generated docs into site/ for GitHub Pages
//! (replaces the Copy-Item step in pages.yml). The mdBook output and rustdoc go
//! under site/book and site/doc, next to the landing page.

use crate::{fsx, paths};
use anyhow::{bail, Context, Result};

pub fn run() -> Result<()> {
    let root = paths::repo_root();
    let pairs = [
        (
            root.join("docs").join("book"),
            root.join("site").join("book"),
        ),
        (
            root.join("engine").join("target").join("doc"),
            root.join("site").join("doc"),
        ),
    ];
    for (src, dst) in &pairs {
        if !src.is_dir() {
            bail!(
                "missing {} — build the docs first (mdbook build docs / cargo doc)",
                src.display()
            );
        }
        fsx::copy_dir_all(src, dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    }
    println!("docs-assemble: staged book + doc into site/");
    Ok(())
}
