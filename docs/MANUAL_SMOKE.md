# Manual smoke checklist

A tight, repeatable manual pass for the parts the automated UI suite
(`app/FindMyFiles.Tests/UiAutomation/ui-tests.ps1`, run via `just ui-test`)
**cannot** cover: the real **UAC consent dialog**, the **post-consent
single-window transition** (the Exhibit-A orphaned-window bug), and the
**no-admin scope path**. UI automation drives a `--fake-engine` process and
cannot click the OS UAC prompt or observe a privileged service install, so these
flows are verified by hand.

Run this before any release, and after touching:
`App.xaml.cs` (incl. `SoftRestart`/`AppReload`), `ServiceProvisioner`,
`ShellOps.Relaunch`/`IAppRestart`, `EngineClientFactory`, `MainWindow`/`MainPage`,
`ScopeManagerDialog`, or `ServiceManagerDialog`.

## Preconditions

- [ ] A **published** bundle exists: `just publish`. The bundle root holds the
      launcher `build/dist/FindMyFiles/FindMyFiles.exe` (what a user runs) +
      `README.txt`; the app itself and the engine binaries live one level down in
      `build/dist/FindMyFiles/app/`.
- [ ] A **clean, non-elevated standard user** (or a fresh local account). The app
      launches `asInvoker` — start it normally, never "Run as administrator".
- [ ] **No fmf-engine service installed** to start (so the first run lands on the
      disconnected setup screen). Verify: `just service-status` reports the
      service absent / stopped, or run `build/dist/FindMyFiles/app/fmf-service.exe status`.
- [ ] You can read the app log. Path depends on the data-root mode:
  - **Portable** (default — the bundle writes beside the exe): `build/dist/FindMyFiles/data/logs/app.log`
  - **Profile**: `%APPDATA%\find-my-files\logs\app.log`

  Quickest open (PowerShell): `notepad (Join-Path $env:APPDATA 'find-my-files\logs\app.log')`
  — if the portable folder exists, read that one instead.

> Tip: clear or note the current end of `app.log` before each section so the
> "no Error lines" checks below only consider lines this run produced.

---

## 1. First-run setup screen renders (no service)

Launch the published exe normally (double-click, or
`Start-Process build\dist\FindMyFiles\FindMyFiles.exe`).

- [ ] Exactly **one** window opens.
- [ ] The **disconnected setup screen** is centered: a title, a body line, the
      **Enable search** accent button, an *or* divider, and the
      **Set up a folder** scope link beneath it.
- [ ] The search box is **not** shown (the search UI is collapsed while
      disconnected).
- [ ] `app.log` has the `launch` line and **no `ERROR` lines** for this run.

---

## 2. Privileged path — real UAC consent + in-process Setup→Ready (Exhibit A)

This is the bug class the automation can't reach. The flow is (ADR-0036):
**Enable search → real UAC prompt → service installs+starts → the app re-resolves
the engine *in-process* (`App.SoftRestartIntoPipe`: rebuild the page onto a
retrying pipe client) — no new process.** The original regression (#107) was that
the old `Process.Start` relaunch redirected back to the still-alive instance under
single-instancing and **both processes exited — the app vanished**; the user had
to reopen it manually. The fix must keep everything in one live window with no
process churn.

### 2a. Consent ACCEPTED

- [ ] Click **Enable search**. A genuine Windows **UAC consent dialog** appears
      (Yes/No), naming `fmf-service.exe` as the elevated action.
- [ ] Click **Yes**. The setup screen shows a progress ring / status text while
      the service registers and the pipe comes up (≈ up to 8 s).
- [ ] The **same window** flips Setup→Ready in place (no flash, no second window,
      no disappearance):
  - [ ] The search box is now visible (engine ready); the setup screen is gone.
  - [ ] **Exactly one FindMyFiles window/process exists throughout** — confirm
        `Get-Process FindMyFiles` shows a **single** process whose **PID is
        unchanged** from before the click (proof it stayed in-process).
  - [ ] The window never vanished — you did **not** have to reopen the app.
- [ ] Type a 3+ char query → real results stream in from the installed service.
- [ ] `app.log` shows the register (`setup … → Ok`) line and **no second `launch`
      line**, and specifically **no `--engine=pipe` launch** — the transition is
      in-process (a `launch` after this point would mean a process relaunch crept
      back, the #107 bug). Also **no `ERROR` lines**. Reading it (PowerShell):
      `Select-String -Path (Join-Path $env:APPDATA 'find-my-files\logs\app.log') -Pattern 'launch|ERROR|--engine=pipe'`
      (or the portable `data\logs\app.log`) — expect only the single startup `launch`.

### 2b. Consent DECLINED

Reset first: `just service-uninstall` (elevated) so the next launch is
disconnected again; relaunch the app.

- [ ] Click **Enable search**, then **No** on the UAC prompt.
- [ ] The app **stays on the setup screen**, still **one window**, fully
      responsive (no hang, no crash, no orphan).
- [ ] A non-fatal notice appears (declined / failed), and **Enable search** is
      clickable again for a retry.
- [ ] `app.log` records the declined outcome at WARN/INFO — **no `ERROR` /
      crash marker** (`crash.marker` must not appear in the logs dir).

---

## 3. No-admin scope path (no elevation at all)

For the locked-down user who can't (or won't) elevate. Start from the
disconnected setup screen again (service uninstalled).

- [ ] Click the **Set up a folder** scope link. The **scope manager dialog**
      opens (folder-only — add folders, optional excludes).
- [ ] Add a small folder you own (e.g. your Documents). Confirm/close the dialog.
- [ ] **No UAC prompt** appears at any point in this path.
- [ ] The app transitions to a **ready** state and indexes the chosen folder;
      a 3+ char query returns matches from within that folder only.
- [ ] **Still exactly one window**; the gear menu shows **Manage scope folders**
      (not the service management item).
- [ ] Re-open the app: it comes back **connected to the same scope** without
      re-prompting (settings persisted).
- [ ] `app.log` has **no `ERROR` lines** for the scope run.

---

## 4. Teardown / log sweep

- [ ] Close the app — exactly one process exits; no FindMyFiles process lingers.
- [ ] If you installed the service for section 2, deregister it:
      `just service-uninstall` (elevated; add `--purge-data` to drop the index).
- [ ] Final log check — open the active `app.log` and confirm the whole session
      produced **no `ERROR` lines** and **no `crash.marker`** in the logs dir.

---

## Result

- [ ] **PASS** — single-window transition held through accept/decline, the
      no-admin scope path needed no elevation, and `app.log` is Error-clean.
- [ ] **FAIL** — record which step, attach the relevant `app.log` excerpt
      (and a window/Task-Manager screenshot for any orphaned-window case).
