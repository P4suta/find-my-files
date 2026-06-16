//! `xtask docs-assemble` — assemble the GitHub Pages site under build/site
//! (replaces the Copy-Item step in pages.yml). The committed landing page
//! (site/) is the base; the mdBook output (build/docs-book), rustdoc
//! (build/engine/doc) and the `DocFX` C# API reference (build/docs-csharp/_site)
//! layer on top as build/site/book, build/site/doc and build/site/api
//! (ADR-0021). pages.yml then uploads build/site as the Pages artifact.

use crate::{fsx, paths};
use anyhow::{bail, Context, Result};

pub fn run() -> Result<()> {
    let root = paths::repo_root();
    let site = paths::site_dir();

    // The committed landing page (site/: index.html, en/, style.css) is the
    // base of the assembled site — copy it into build/site first, then layer
    // the generated docs on top. site/ is source, build/site is the output.
    let landing = root.join("site");
    if !landing.is_dir() {
        bail!("missing {} — the committed landing page", landing.display());
    }
    fsx::copy_dir_all(&landing, &site)
        .with_context(|| format!("copy {} -> {}", landing.display(), site.display()))?;

    // mdBook output + rustdoc + DocFX C# API — must be built first (`just doc`).
    let pairs = [
        (paths::build_root().join("docs-book"), site.join("book")),
        (
            paths::build_root().join("engine").join("doc"),
            site.join("doc"),
        ),
        (
            paths::build_root().join("docs-csharp").join("_site"),
            site.join("api"),
        ),
    ];
    for (src, dst) in &pairs {
        if !src.is_dir() {
            bail!(
                "missing {} — build the docs first (just doc)",
                src.display()
            );
        }
        fsx::copy_dir_all(src, dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    }
    println!("docs-assemble: assembled landing + book + doc into build/site/");
    Ok(())
}
