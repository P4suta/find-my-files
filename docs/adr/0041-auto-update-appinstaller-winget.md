# ADR-0041: Auto-update via App Installer (.appinstaller) + winget — no bespoke updater

Date: 2026-07-07 / Status: Proposed (decision-ready; implementation is the M-B follow-up to ADR-0028)

## Decision

Deliver auto-update by riding **platform-native mechanisms only** — write **no**
update-check / download / apply code of our own:

- **App Installer `.appinstaller`** (the sideload/direct-download channel): ship a
  small XML manifest next to the `.msix` on GitHub Releases that points at a stable
  URI and declares `<UpdateSettings>` (`OnLaunch` with `HoursBetweenUpdateChecks`
  + `AutomaticBackgroundTask`). The Windows-built-in App Installer polls the URI
  and updates the packaged UI silently in the background / on launch.
- **winget** (the discovery/package-manager channel): submit the signed `.msix` +
  its SHA256 to `microsoft/winget-pkgs` (a YAML manifest pointing at the Release
  asset — no Store account). `winget upgrade` then updates it by the standard path.

This is the M-B milestone; ADR-0028 (M-A) already ships the signed `.msix` with the
numeric-monotonic `X.Y.Z.0` version `.appinstaller` requires. No engine / wire /
contract / golden change.

## Context

The portable zip has no update story (manual re-download). ADR-0028 gives us the
`.msix`, which unlocks the two mechanisms Windows already provides for keeping a
packaged app current. The project's standing direction is to **follow industry
standards rather than build something original** for distribution/update, and its
design ethos (DSA-first, not bolt-on) rejects carrying a bespoke updater's attack
surface and elevation questions when the platform does it for free.

## Considered alternatives

- **Bespoke in-app updater** (poll the GitHub Releases API → download → self-replace)
  — **REJECTED.** New network + code-execution attack surface, a self-replacement /
  elevation path to get right, and update UX to maintain — all to reimplement what
  App Installer + winget already do. Contradicts the platform-native remit.
- **winget only** — **REJECTED as the sole mechanism.** `winget upgrade` is
  user/scheduler-invoked, not silent-automatic; kept as the discovery channel, not
  the auto-update one.
- **Microsoft Store** — **DEFERRED.** Broadest auto-update reach, but Store
  onboarding + review + the packaged-identity constraints are out of scope for now;
  the self-hosted `.appinstaller` + winget path needs no Store account.

## Consequences

- **New surface**: an `xtask` verb emitting the `.appinstaller` XML (stable
  `.../releases/latest/download/…` URIs, `Publisher`/`Version` matching the
  package), release.yml attaching it, and a winget-pkgs submission manifest. No app
  code.
- **Service version skew** (the one real wrinkle): App Installer updates only the
  packaged UI; the out-of-band `fmf-service.exe` in `%ProgramData%` is not touched.
  The existing handling suffices — the pipe Hello handshake negotiates the ABI, the
  in-app `HasVersionMismatch` warning surfaces a stale pair, and the next
  service-setup re-register self-heals it (copying the newer bundled exe). Only an
  update that changes the **engine ABI** needs a forced service re-setup prompt;
  call that out in the implementation.
- **Invariant to preserve** (already honoured by ADR-0028): `.appinstaller` requires
  strictly increasing numeric versions, which `version::msix_version`'s stable-only
  `X.Y.Z.0` guarantees.

## Re-examination triggers

- **App Installer background updates prove unreliable** in the field → add an
  in-app "check for updates" affordance that calls the App Installer API (still not
  a bespoke downloader).
- **A need to auto-update the service in lock-step with the UI** → design a
  service-update path (today the daily GC + re-register covers correctness, not
  freshness).
- **Microsoft Store distribution becomes desirable** → revisit Store submission,
  which brings its own auto-update.
