//! `xtask docs-assemble` — assemble the GitHub Pages site under build/site
//! (replaces the Copy-Item step in pages.yml). The committed landing page
//! (site/) is the base; the mdBook output (build/docs-book) and rustdoc
//! (build/engine/doc) layer on top as build/site/book and build/site/doc, and
//! the C# API reference (build/docs-csharp/_site), when present, as
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

    // `cargo doc` on a multi-crate workspace writes no root index.html (only
    // per-crate dirs like fmf_core/index.html), so a bare /doc/ would 404. Drop
    // a redirect to the core crate; rustdoc's crate dropdown reaches the rest.
    let doc_index = site.join("doc").join("index.html");
    std::fs::write(&doc_index, doc_index_redirect_html("fmf_core"))
        .with_context(|| format!("write {}", doc_index.display()))?;

    // The C# API reference is optional: when `just doc-csharp` (DefaultDocumentation
    // -> mdBook) hasn't been run, or it failed, build/docs-csharp/_site is absent
    // and we omit the api/ section with a loud warning rather than failing the
    // whole deploy. pages.yml runs doc-csharp before this, so /api/ is populated.
    let api_src = paths::build_root().join("docs-csharp").join("_site");
    let api_dst = site.join("api");
    if api_src.is_dir() {
        fsx::copy_dir_all(&api_src, &api_dst)
            .with_context(|| format!("copy {} -> {}", api_src.display(), api_dst.display()))?;
        println!("docs-assemble: assembled landing + book + doc + api into build/site/");
    } else {
        eprintln!(
            "docs-assemble: WARNING — {} missing; omitting the C# API reference \
             (run `just doc-csharp` first)",
            api_src.display()
        );
        println!("docs-assemble: assembled landing + book + doc into build/site/ (api omitted)");
    }
    Ok(())
}

/// The redirect page written at the rustdoc output root (build/site/doc/
/// index.html). `cargo doc` on a multi-crate workspace emits no top-level
/// index, so /doc/ would 404; this meta-refreshes to the core crate, from which
/// rustdoc's crate dropdown reaches the rest. Returns the page so the markup is
/// unit-tested without touching the filesystem.
fn doc_index_redirect_html(crate_name: &str) -> String {
    format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta http-equiv=\"refresh\" content=\"0; url={crate_name}/index.html\">\n\
         <link rel=\"canonical\" href=\"{crate_name}/index.html\">\n\
         <title>find-my-files — Rust API</title>\n\
         </head>\n\
         <body>\n\
         <p>Redirecting to the <a href=\"{crate_name}/index.html\">find-my-files Rust API</a>…</p>\n\
         </body>\n\
         </html>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_redirect_points_at_the_named_crate() {
        let html = doc_index_redirect_html("fmf_core");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("content=\"0; url=fmf_core/index.html\""));
        assert!(html.contains("href=\"fmf_core/index.html\""));
    }
}
