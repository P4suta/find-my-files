//! Embeds the application icon into the launcher executable so the bundle's
//! top-level `FindMyFiles.exe` is visually identical to the app it starts.
//!
//! Best-effort by design: if the resource compiler is unavailable the build
//! still succeeds (the launcher just keeps the default icon). A cosmetic must
//! never break the build — and CI (windows-latest, full SDK) always has the
//! compiler, so the shipped release artifact gets the icon regardless.

fn main() {
    // The canonical icon is the app's own, referenced directly to avoid a
    // second copy that could drift. Path is relative to this crate dir
    // (engine/crates/fmf-launcher), which is build.rs's working directory.
    const ICON: &str = "../../../app/FindMyFiles/Assets/AppIcon.ico";
    println!("cargo:rerun-if-changed={ICON}");

    // Resources are a Windows-only concept; the whole project is Windows-only,
    // but guard anyway so a non-Windows `cargo check` stays clean.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON);
    if let Err(e) = res.compile() {
        println!("cargo:warning=fmf-launcher: icon not embedded ({e})");
    }
}
