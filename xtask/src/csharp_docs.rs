//! `xtask doc-csharp` — generate the C# API reference (build/docs-csharp/_site,
//! published at /api/).
//!
//! `DefaultDocumentation` reads the BUILT `FindMyFiles.dll` + its co-located XML
//! doc through IL (`ICSharpCode.Decompiler`), so it works with the current .NET
//! 10 SDK where `DocFX`'s Roslyn metadata path extracts zero types (docfx#11046
//! / #40). It emits Markdown, which we render to HTML with `mdBook` — the same
//! renderer as the design docs (docs/), so /api/ matches /book/. The caller
//! (`just doc-csharp`) builds the DLL and restores the dotnet tool first;
//! everything here lands under build/ (ADR-0021).

use crate::{cmd, fsx, paths};
use anyhow::{bail, Context, Result};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

pub fn run() -> Result<()> {
    let root = paths::repo_root();
    let dll = locate_assembly()?;
    let book = paths::build_root().join("docs-csharp").join("book");
    let src = book.join("src");

    // Deterministic output: wipe the previous book so stale type pages never
    // linger (the public API surface shrinks as well as grows).
    if book.is_dir() {
        fsx::force_remove_dir_all(&book).with_context(|| format!("clear {}", book.display()))?;
    }
    fs::create_dir_all(&src).with_context(|| format!("create {}", src.display()))?;

    // Public surface only (drops the CommunityToolkit source-generated
    // __Internals types compiled into the assembly); one page per type with
    // members inline — per-member pages put whole method signatures in file
    // names and blow past the Windows path limit. XML doc sits next to the DLL.
    let dll = dll.to_str().context("non-UTF-8 assembly path")?;
    let src_arg = src.to_str().context("non-UTF-8 src path")?;
    cmd::run(
        &root,
        "dotnet",
        &[
            "defaultdocumentation",
            "-a",
            dll,
            "-o",
            src_arg,
            "--GeneratedAccessModifiers",
            "Public",
            "--GeneratedPages",
            "Namespaces,Types",
            "--AssemblyPageName",
            "index",
        ],
    )?;

    // mdBook only renders pages listed in SUMMARY.md; DefaultDocumentation emits
    // none, so build it from the generated files.
    let mut pages: Vec<String> = fs::read_dir(&src)
        .with_context(|| format!("read {}", src.display()))?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            std::path::Path::new(n)
                .extension()
                .is_some_and(|e| e == "md")
                && n != "SUMMARY.md"
        })
        .collect();
    pages.sort();
    fs::write(src.join("SUMMARY.md"), build_summary(&pages)).context("write SUMMARY.md")?;
    fs::write(book.join("book.toml"), BOOK_TOML).context("write book.toml")?;

    // Render to build/docs-csharp/_site (book.toml build-dir = ../_site), the
    // directory docs-assemble stages into /api/.
    let book_arg = book.to_str().context("non-UTF-8 book path")?;
    cmd::run(&root, "mdbook", &["build", book_arg])?;

    let count = pages.len();
    println!("doc-csharp: rendered {count} C# API pages into build/docs-csharp/_site");
    Ok(())
}

/// Find the built `WinUI` assembly: build/app/FindMyFiles/Release/&lt;tfm&gt;/
/// win-x64/`FindMyFiles.dll`. The tfm folder name carries the Windows SDK
/// version, so it is discovered, not hard-coded.
fn locate_assembly() -> Result<PathBuf> {
    let release = paths::build_root()
        .join("app")
        .join("FindMyFiles")
        .join("Release");
    let entries = fs::read_dir(&release).with_context(|| {
        format!(
            "read {} — build the app first (`dotnet build app/FindMyFiles -c Release`)",
            release.display()
        )
    })?;
    for entry in entries.filter_map(Result::ok) {
        let dll = entry.path().join("win-x64").join("FindMyFiles.dll");
        if dll.is_file() {
            return Ok(dll);
        }
    }
    bail!(
        "FindMyFiles.dll not found under {} — build the app first",
        release.display()
    )
}

/// `mdBook` `SUMMARY.md` for the generated pages: the assembly index as the root
/// (prefix chapter), then a flat, sorted list of the namespace + type pages.
/// `DefaultDocumentation`'s dot-names sort so each namespace clusters above its
/// types. `pages` must be sorted and exclude `SUMMARY.md`.
fn build_summary(pages: &[String]) -> String {
    let mut s = String::from("# Summary\n\n[find-my-files — C# API](index.md)\n\n");
    for p in pages {
        if p == "index.md" {
            continue; // the root prefix chapter above, never a list item
        }
        let title = p.strip_suffix(".md").unwrap_or(p);
        // Writing to a String is infallible.
        let _ = writeln!(s, "- [{title}]({p})");
    }
    s
}

/// `mdBook` config for the API book. Mirrors `docs/book.toml`'s theme so /api/
/// and /book/ look alike; build-dir is relative to this file (build/docs-csharp/
/// book/), so ../_site resolves to build/docs-csharp/_site (ADR-0021).
const BOOK_TOML: &str = "\
[book]
title = \"find-my-files — C# API\"
language = \"en\"
src = \"src\"

[build]
build-dir = \"../_site\"

[output.html]
default-theme = \"navy\"
preferred-dark-theme = \"navy\"
git-repository-url = \"https://github.com/P4suta/find-my-files\"
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_roots_at_index_and_lists_the_rest_sorted() {
        let pages = vec![
            "FindMyFiles.App.md".to_string(),
            "FindMyFiles.Engine.IEngineClient.md".to_string(),
            "index.md".to_string(),
        ];
        let s = build_summary(&pages);
        // index.md is the prefix-chapter root, not a numbered list item.
        assert!(s.contains("[find-my-files — C# API](index.md)"));
        assert!(!s.contains("- [index]"));
        assert!(s.contains("- [FindMyFiles.App](FindMyFiles.App.md)"));
        assert!(
            s.contains("- [FindMyFiles.Engine.IEngineClient](FindMyFiles.Engine.IEngineClient.md)")
        );
    }
}
