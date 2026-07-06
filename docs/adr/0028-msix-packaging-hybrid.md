# ADR-0028: MSIX packaging — hybrid (packaged UI, unpackaged service)

Date: 2026-06-24 / Status: **Rejected** (2026-07-07 — MSIX distribution has no viable signing/store path for this app; see *Rejection* below). The original hybrid design (Decision et seq.) is retained as the record of what was evaluated and built.

## Rejection (2026-07-07)

The hybrid was **implemented and validated**, then **rejected at the distribution boundary**: there is
no way to *sign and ship* an MSIX for this app that is simultaneously (a) an **official** signing tool,
(b) automatable in **CI**, (c) compatible with our **Individual Validation (IV)** certificate (ADR-0020),
and (d) able to sign **MSIX**. Every candidate failed on at least one axis — and independently, the
Microsoft Store path is structurally unfit for a service-backed app:

- **SSL.com CodeSignTool** (official, IV-ok, CI-automatable) — **cannot sign MSIX**. CI log:
  `Error: Unsupported file format for signing - msix`. It signs PEs/MSI, not MSIX packages.
- **SSL.com eSigner CKA** (official, MSIX-capable via signtool) — **fails in CI**. Already recorded in
  ADR-0029: 3 dry-runs ending in `SignerSign() failed (0x80090003)` / "Signing credentials not
  configured" in the KSP credential path. SSL.com further documents CKA's **automated (OTP-less) mode as
  OV/EV-only** (unverified, ssl.com unreachable), which would exclude our IV cert regardless; either way
  CKA did not work with this cert.
- **jsign** (would sign MSIX with the IV cert via the CSC API) — **rejected on policy**: a tool that
  handles the signing key must be **official vendor tooling**, not third-party OSS.
- **Microsoft Store** (Microsoft signs the package — no own cert needed) — **structurally unfit**. Store
  Policy 7.19 §10.x: non-Microsoft **NT service** dependencies are "generally not allowed" (case-by-case,
  requires disclosure). Our `fmf-service` is a **LocalSystem SCM service** that reads raw MFT/USN and
  **survives uninstall** — the antithesis of the Store's package-lifecycle model. Estimated certification
  odds ~15% and fragile; MFT/USN-class search utilities are effectively **absent from the Store** (they
  ship their own installers), which confirms the mismatch. Individual accounts also likely cannot use the
  automated submission API (needs an org + Azure AD association).

**What DID work** (so the effort is on record and reusable): `xtask package-msix` built a valid `.msix`
**in CI on windows-latest** (MakePri/MakeAppx resolved from the pinned SDK BuildTools NuGet), and locally
the `.msix` was **self-signed → installed → launched → searched** successfully, confirming **R3** (bundled
service-exe resolution) and **R4** (profile-path forcing under package identity) on a real machine. The
packaging itself is sound; only the **distribution-layer signing** has no official path today.

**Decision**: reject MSIX distribution. The **signed portable zip (ADR-0021)** remains the single official
channel — the same choice every comparable MFT/USN tool makes. The implementation is preserved on the
annotated tag **`archive/msix-attempt-2026-07`** (branch `feat/msix-packaging` was deleted). The
`.appinstaller` + winget auto-update follow-up sketched during this work is **moot** (it presupposed a
shippable MSIX).

**Re-examination triggers** (any one revives this):
1. **Azure Trusted Signing opens to individuals in Japan** (ties to ADR-0020's trigger) — a managed,
   MSIX-capable signer reachable from CI without a hardware token.
2. **SSL.com documents a working unattended IV × MSIX recipe**, or eSigner CKA's KSP credential path is
   fixed (ADR-0029's own trigger).
3. **The app drops the privileged-service dependency** (e.g. a fully non-elevated engine), which would
   make the Microsoft Store a clean fit.

The original hybrid design and its rejected alternatives follow, unchanged, as the evaluated record.

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
