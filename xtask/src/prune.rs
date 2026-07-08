//! Publish artifacts to strip from the distributable bundle.
//!
//! `dotnet publish` copies files into `build/dist/FindMyFiles/app` that the app
//! ships but never loads at runtime: debug symbols, the internal XML doc, a
//! design-time-only helper, and the `WebView2` payload the `WinUI` component
//! package drags in transitively. The publish step deletes these after the
//! locale prune and before the bundle self-verify. This module is the single
//! pure source of truth for *which* paths (relative to the `app/` payload) get
//! stripped — the `verify_bundle` self-check asserts none of them survive, so
//! the pruner and the guard can never drift.

/// Build/IDE-only files that are always safe to drop.
///
/// `FindMyFiles.pdb` is debug symbols; `FindMyFiles.xml` is the generated XML
/// doc — both are build artifacts, not shipping payload (`GenerateDocumentationFile`
/// is on to enforce IDE0005/CS1591 in the build, see `FindMyFiles.csproj`, not to
/// ship API docs). `Microsoft.UI.Designer.dll` is the Visual Studio XAML
/// design-time helper — never loaded by the running app.
pub const ALWAYS_PRUNE: &[&str] = &[
    "FindMyFiles.pdb",
    "FindMyFiles.xml",
    "Microsoft.UI.Designer.dll",
];

/// `WebView2` assemblies, pulled in transitively by `Microsoft.WindowsAppSDK.WinUI`
/// (see `packages.lock.json`). The app references no `Microsoft.Web.WebView2`
/// type — a filename search renders no web content — so nothing loads them.
/// Verified removable this session: with these gone the app still builds,
/// launches, and passes `test-app` plus a UI smoke.
pub const WEBVIEW2: &[&str] = &[
    "Microsoft.Web.WebView2.Core.dll",
    "Microsoft.Web.WebView2.Core.Projection.dll",
    "WebView2Loader.dll",
];

/// Every artifact the publish step strips, `app/`-relative. The single set shared
/// by the pruner (`publish::prune_publish_artifacts`) and the bundle self-verify
/// (`publish::verify_bundle`), so a file can't be pruned yet still asserted, or
/// vice-versa.
pub fn shipped_prune_set() -> impl Iterator<Item = &'static str> {
    ALWAYS_PRUNE.iter().copied().chain(WEBVIEW2.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_pruned(name: &str) -> bool {
        shipped_prune_set().any(|p| p == name)
    }

    #[test]
    fn prunes_the_known_dead_weight() {
        for dead in [
            "FindMyFiles.pdb",
            "FindMyFiles.xml",
            "Microsoft.UI.Designer.dll",
            "Microsoft.Web.WebView2.Core.dll",
            "Microsoft.Web.WebView2.Core.Projection.dll",
            "WebView2Loader.dll",
        ] {
            assert!(is_pruned(dead), "{dead} must be pruned");
        }
    }

    #[test]
    fn never_prunes_a_launch_critical_file() {
        // The bundle must never strip anything the app needs to run — a typo in
        // the prune list that hit one of these would ship a broken zip.
        for keep in [
            "FindMyFiles.exe",
            "FindMyFiles.dll",
            "WinRT.Runtime.dll",
            "coreclr.dll",
            "hostfxr.dll",
            "fmf_engine.dll",
            "fmf-service.exe",
            "fmf.exe",
            "Microsoft.ui.xaml.dll",
        ] {
            assert!(!is_pruned(keep), "{keep} must be kept");
        }
    }

    #[test]
    fn the_set_has_no_duplicates() {
        let all: Vec<&str> = shipped_prune_set().collect();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert!(!a.eq_ignore_ascii_case(b), "duplicate prune entry: {a}");
            }
        }
    }
}
