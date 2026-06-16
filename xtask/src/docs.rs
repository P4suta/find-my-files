//! `xtask docs-assemble` — assemble the GitHub Pages site under build/site
//! (replaces the Copy-Item step in pages.yml). The committed landing page
//! (site/) is the base; the mdBook output (build/docs-book) and rustdoc
//! (build/engine/doc) layer on top as build/site/book and build/site/doc, and
//! the `DocFX` C# API reference (build/docs-csharp/_site), when present, as
//! build/site/api (ADR-0021). pages.yml then uploads build/site as the Pages
//! artifact.

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

    // mdBook output + rustdoc are required (`just doc`).
    let required = [
        (paths::build_root().join("docs-book"), site.join("book")),
        (
            paths::build_root().join("engine").join("doc"),
            site.join("doc"),
        ),
    ];
    for (src, dst) in &required {
        if !src.is_dir() {
            bail!(
                "missing {} — build the docs first (just doc)",
                src.display()
            );
        }
        fsx::copy_dir_all(src, dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
    }

    // The DocFX C# API reference is optional: DocFX 2.78.5 (the latest release)
    // extracts zero types from assemblies built by the current .NET 10 SDK, so
    // when its output is absent we omit the api/ section with a loud warning
    // rather than failing the whole deploy. It returns automatically once DocFX
    // can read the SDK's output (tracked in #40).
    let api_src = paths::build_root().join("docs-csharp").join("_site");
    let api_dst = site.join("api");
    if api_src.is_dir() {
        fsx::copy_dir_all(&api_src, &api_dst)
            .with_context(|| format!("copy {} -> {}", api_src.display(), api_dst.display()))?;
        println!("docs-assemble: assembled landing + book + doc + api into build/site/");
    } else {
        eprintln!(
            "docs-assemble: WARNING — {} missing; omitting the C# API reference \
             (DocFX cannot read the current .NET 10 SDK output; see #40)",
            api_src.display()
        );
        println!("docs-assemble: assembled landing + book + doc into build/site/ (api omitted)");
    }
    Ok(())
}
