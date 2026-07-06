# ADR-0028: MSIX packaging — hybrid (packaged UI, unpackaged service)

Date: 2026-06-24 / Status: Accepted (implemented 2026-07-07 — see *Implementation update* below)

## Implementation update (2026-07-07)

The hybrid landed as specified, with these decisions made concrete during
implementation:

- **R3 resolved — `fmf-service.exe` ships as a plain CONTENT payload** in the
  `.msix` (a file, NOT a `desktop6:Service`). This SUPERSEDES the Decision's
  "package contains `FindMyFiles.exe` + `fmf_engine.dll` only" line: bundling the
  service exe as content is what gives the read-only-`WindowsApps` UI a defined
  source to copy into `%ProgramData%`. Single-project MSIX's "one executable" rule
  is about the *registered* app entry point (still just the apphost); extra loose
  exes are allowed as content. `ServiceSetup.LocateServiceExe` gained a packaged
  branch resolving it from `Package.Current.InstalledLocation`. `fmf.exe` (CLI) and
  the zip launcher remain excluded.
- **Build path — `xtask package-msix <tag>`**, out-of-band with the standard
  Windows SDK tools (`MakePri` + `MakeAppx`) sourced from the already-pinned
  `Microsoft.Windows.SDK.BuildTools` NuGet package. The csproj's
  `WindowsPackageType=None` is untouched (CLAUDE.md / ADR-0016). MSIX ships **stable
  only**: the version is the 4-part numeric `X.Y.Z.0` (`version::msix_version`).
- **Manifest** at `packaging/msix/Package.appxmanifest.in` (+ generated `Assets/`),
  `Publisher` pinned verbatim to the SSL.com IV cert subject (`xtask` asserts it),
  `runFullTrust` only, **no** `desktop6:Service`.
- **R4/R2** — `PackageIdentity.IsPackaged` (via `GetCurrentPackageFullName`) forces
  the profile path under identity in `AppPaths` (portable `<exe>\data` disabled). A
  zip→msix user on the profile-fallback path migrates transparently via the OS's
  copy-on-write read-through of `%APPDATA%`.
- **CI** — the `sign` job packs the `.msix` from the signed bundle and
  Authenticode-signs the wrapper with the same eSigner Action (kept in that job so
  signing secrets never spread); the shared `verify-signatures` composite gained an
  `msix-path` input so the sign self-check and the publish gate both verify it; the
  `publish` job checksums, attests, and attaches it beside the zip.
- **Runtime auto-update** (App Installer `.appinstaller` + winget) is deferred to
  **ADR-0041** — the numeric-monotonic `X.Y.Z.0` version is chosen now so that
  channel can bolt on without a version rework.

The original decision and rejected alternatives (still valid) follow.

## Decision

Package **only the UI** as a **self-contained, single-project MSIX**, and keep the privileged
**service unpackaged**, installed exactly as today via `fmf-service install` (the one-time
elevation flow of ADR-0027). Distribute the signed `.msix` (App Installer double-click + winget)
**alongside** the unchanged portable zip (ADR-0021), not instead of it.

This amends ADR-0016's "MSIX/installer deferred" stance and **leaves the ADR-0017 / ADR-0027
service-security model untouched**. The csproj's default `WindowsPackageType=None` build is **not
flipped** (CLAUDE.md / ADR-0016 forbid reverting it) — the MSIX is produced by a separate
packaging path, not by mutating the portable build.

The MSIX contains `FindMyFiles.exe` (the apphost) + `fmf_engine.dll` only. `fmf.exe` and
`fmf-service.exe` are **not** in the package; the elevated install copies `fmf-service.exe` into
`%ProgramData%\find-my-files` exactly as ADR-0027 already does.

Implementation is deferred: this ADR records the decision, the rejected alternatives, and the
runbook so the work is decision-ready. Verified external facts (with sources) live in
`docs/RESEARCH.md` (§ MSIX packaging).

## Context

The portable zip is the current channel. ADR-0021's launcher restructure (root `FindMyFiles.exe`
+ `README.txt` + `app/`) fixed the "which file do I run" pain *for the zip*. MSIX is wanted for the
**installed** experience the zip cannot give: a Start-menu identity (the user never sees the file
sea), clean install/uninstall, App Installer trust, and `winget` distribution. Code signing is
already active (ADR-0020, SSL.com Individual Validation), which is the MSIX publisher prerequisite.

Two **decisive external constraints** (RESEARCH.md § MSIX) shape the decision:

1. **The MSIX service extension (`desktop6:Service`) exposes none of our security controls.** Its
   only attributes are `Name` / `StartupType` (auto|manual|disabled) / `StartAccount`
   (localSystem|localService|networkService) / `Arguments` + child dependencies/triggers. There is
   **no** attribute for the service-object DACL, `SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO` privilege
   stripping, `SERVICE_PRESHUTDOWN_INFO`, install-time SID capture, `%ProgramData%` DACL hardening,
   or a scheduled GC task. Install/uninstall is owned by the MSIX deployment engine.
2. **Single-project MSIX bundles exactly one executable.** Putting all three of our exes in one
   package would require a classic Windows Application Packaging Project (`.wapproj`).

Together these mean a *packaged service* is incompatible with the ADR-0017/0027 hardening, and a
*single-project* package cannot hold the CLI + service anyway. The hybrid sidesteps both: the
service stays a normal SCM service installed by our own elevated helper (which, launched from the
packaged UI, runs **without package identity** — full-trust, free to write `%ProgramData%`, register
the SCM service, set DACLs, strip privileges, and create the GC task; RESEARCH.md § MSIX).

## Considered alternatives

- **Full MSIX with a packaged `desktop6:Service` — REJECTED.** Loses **every** ADR-0017/0027 control
  at once: no service-object DACL (re-opens the LPE vector, ADR-0027 Threat 9), no privilege
  stripping (ADR-0017), no SID capture / pipe-SDDL owner check (Threat 1), no hardened
  `%ProgramData%` DACL (Threat 7, `.fmfidx` leak), and the service exe pinned read-only inside
  `WindowsApps` tied to the package lifecycle (breaks the stable-copy anti-tamper of Threat 10 and
  portable-app-deletion survival). Also needs a `.wapproj` for three exes. A security regression in
  the project's most safety-critical component — unacceptable.
- **Zip-only (status quo) — REJECTED for the milestone, retained as a channel.** No clean uninstall,
  no App Installer trust UX, and portable/zip is not accepted for `winget-pkgs` community submission
  (MSIX is the safe winget type). Kept as the **secondary** portable channel (it is also the only
  `PublishSingleFile`-eligible shape and serves users who refuse installers), but not the only one.
- **Framework-dependent MSIX — REJECTED as default.** Smaller package, but requires the Windows App
  Runtime present on the target; there is no Store dependency resolver on the sideload / winget
  self-host path, so it adds install friction. Self-contained matches the project's existing
  `WindowsAppSDKSelfContained=true` ethos and installs with nothing pre-present. Kept as the
  re-examination trigger if package size becomes a complaint.
- **`.wapproj` multi-exe MSIX (UI + CLI + service all packaged) — REJECTED.** Heavier project model
  than the single-project tooling the repo already references, and still inherits the packaged-service
  security loss above for the service component.

## Consequences

- **Three artifacts**: `.msix` (App Installer + winget), the portable `.zip` (unchanged), and the
  engine tools (CLI/service) shipped inside the zip / as the install source for the service.
- **New surface**: a `Package.appxmanifest` (`Publisher` = the SSL.com IV cert Subject DN *exactly*,
  `runFullTrust`, **no** `desktop6:Service`), visual assets, a CI step to build + sign the `.msix`
  (folded into the same `SSLcom/esigner-codesign` signing path as the binaries — ADR-0029 — the Action
  signs `.msix` directly) with a `Publisher == cert Subject` assertion, and an `xtask package-msix` verb
  sourced from `paths.rs`. App code: package-identity detection in `AppPaths` (force the profile path, disable
  portable `<exe>\data` under MSIX), a packaged-mode source for `fmf-service.exe`, and a settings
  migration.
- **Residual risks (must be tracked):**
  - **R1 — MSIX uninstall cannot reap the service.** Removing the UI package does not run
    `fmf-service uninstall`; the orphan is reaped by ADR-0027's daily GC (designed for exactly the
    deleted-app case) or the in-app "Remove" button. Acceptable, but called out.
  - **R2 — Settings migration.** A packaged process's `%APPDATA%` writes are copy-on-write redirected
    to the per-package container, so a zip→MSIX user sees a fresh profile unless settings are migrated
    or the path is forced.
  - **R3 — Service-exe provenance when packaged.** The UI under read-only `WindowsApps` must locate an
    `fmf-service.exe` to copy into `%ProgramData%`; `ServiceSetup.LocateServiceExe`'s next-to-exe and
    dev-tree probes do not resolve there, so the packaged build needs a defined source. **The main new
    engineering item.**
  - **R4 — No portable mode when packaged** (`WindowsApps` is read-only) — the packaged UI must use the
    profile path.
- **No wire-contract / ABI / engine change.** This is purely a packaging/distribution decision.

## Re-examination triggers

- **Azure Trusted Signing opens to individuals in Japan** → revisit the MSIX signing provider (ties to
  ADR-0020's trigger).
- **MSIX package size becomes a user complaint** → evaluate the framework-dependent variant.
- **`winget-pkgs` begins accepting zip/portable** for community submission → reconsider whether MSIX is
  still required for winget.
- **Users report orphaned services after MSIX uninstall** (R1) → consider a packaged uninstall hook or a
  more aggressive GC cadence.
- **A need arises to ship the CLI/service inside the package** → revisit the `.wapproj` multi-exe shape,
  re-weighing the packaged-service security loss.
