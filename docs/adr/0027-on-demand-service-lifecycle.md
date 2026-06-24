# ADR-0027: On-demand service lifecycle (manual start + idle stop + idle GC)

Date: 2026-06-23 / Status: Accepted (amends the "resident service" lifecycle decision of [ADR-0016](0016-service-split-named-pipe.md); the service split, transport, and security model are unchanged)

## Decision

Stop running `fmf-engine` as a boot-time resident. Instead:

1. **Manual start** — register the service `SERVICE_DEMAND_START` (was `SERVICE_AUTO_START` + delayed). It no longer starts at every boot; it runs only when something starts it.
2. **Unelevated start/stop** — at install (one-time, elevated) set the *service-object* DACL to grant the authorized user SID(s) `SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS` (and read), so the asInvoker app starts the service on launch with **no UAC**. Never grant a standard user `SERVICE_CHANGE_CONFIG` / `DELETE` / `WRITE_DAC` / `WRITE_OWNER` — on a LocalSystem service that is local privilege escalation.
3. **App-launch start** — `EngineClientFactory.DecideAuto` routes an installed-but-stopped service to `StartThenPipe`; `Resolve` runs `fmf-service start` unelevated, then connects over the pipe (the existing supervisor retry waits for it to come up). A start that cannot be performed (an older install without the granted right) falls back to the setup screen, whose re-register migrates it.
4. **Idle self-stop** — `serve()` stops itself after `service.json` `idle_stop_secs` (default **300 = 5 min**) with no live pipe connection. The clock starts only after a client has connected and dropped; a self-stop is held off while an initial scan is in flight. `0` disables it (the legacy "stay resident once started" behaviour).
5. **Idle GC** — a daily SYSTEM Scheduled Task runs `fmf-service gc`, which uninstalls the service + removes the task + purges the data when `last_use` is older than `gc_max_idle_days` (default **7**, `0` disables). To survive the portable app folder being deleted, install copies `fmf-service.exe` into the hardened data root (`%ProgramData%\find-my-files\fmf-service.exe`) and points the registration and the task at that copy.

## Rationale

- ADR-0016 chose a resident service so "the index stays fresh via USN tracking even when the UI is not running." That is real, but this is a momentary-use tool: a permanent boot-time process holding the index in RAM forever does not match how it is used. The owner's call is **minimal footprint over an always-hot index**.
- A `DEMAND_START` service that is stopped consumes **zero RAM and zero CPU** — it is just an inert SCM row. So manual-start + idle-stop *fully* solves the "resident forever, eats memory" concern; the idle GC is housekeeping (it removes the leftover registration/data and self-heals an orphaned install after the portable app is deleted — there is no installer/uninstaller to do it).
- Granting the user `SERVICE_START`/`STOP` on the service object is what makes the per-launch start UAC-free. It is a deliberate, minimal widening of the service ACL; the dangerous rights stay admin-only (see threats in docs/SECURITY.md).
- A stopped service cannot run a timer, so the week GC must be driven by an external scheduler. A Scheduled Task is the standard Windows mechanism and is far lighter than a resident process (it runs for milliseconds, only when it fires, and removes itself when the GC completes).

### Trade-off

Abandoning residence means a cold start at the first search of a session: the snapshot is loaded and the USN journal replayed; if the journal has wrapped since the last run (long absence, heavy churn) it is a full rescan. Measured baselines (ADR-0016): restore→ready p50 108 ms, ~1.25 s including process spawn; full rescan ≈5 s/250k, ≈60 s/1M. **Hot search p99 (<10 ms) is unchanged** — the only new cost is one cold start per session.

### Rejected alternatives

- **In-proc only while the app is open (no service)** — MFT/USN reads need elevation, so this is a UAC prompt on *every* launch; the non-elevated scope walk (ADR-0024) cannot do whole-C: + USN. One-time install then UAC-free on-demand start is strictly better UX.
- **Self-uninstall on idle instead of a Scheduled Task** — a service that idle-stops after 5 min is never running at the 1-week mark to notice the absence. Time-based GC fundamentally needs an external scheduler.
- **GC task / service pointing at the portable exe** — deleting the app folder breaks both, leaving an un-GC-able orphan. The stable copy in `%ProgramData%` is what makes "auto-delete after a week" actually work for the deleted-app case (and fixes the latent bug where the resident service's binary path pointed into a deletable folder). Version skew between bundle and copy is detected by the pipe Hello handshake and self-heals on the next re-register.

## Consequences

- No wire-contract / golden / ABI change: everything is SCM-, filesystem-, and Scheduled-Task-level, plus two additive `service.json` fields (`idle_stop_secs`, `gc_max_idle_days`) read with serde `#[serde(default)]`. Observability is via `engine.log` (idle stop) and `app.log` (on-demand start), not new counters (an idle-stop counter dies with the process).
- Install now copies a binary into `%ProgramData%` and registers a Scheduled Task; both are removed on teardown (see the cleanup guarantee below).
- **Machine footprint & cleanup guarantee.** The whole footprint is: the SCM service key (`HKLM\…\Services\fmf-engine`), the data dir `%ProgramData%\find-my-files\` (index/logs/`service.json`/`last_use` + the stable `fmf-service.exe`), and the GC Scheduled Task (`System32\Tasks` + HKLM TaskCache). Nothing else is touched — no HKCU, no Program Files, no Start-menu/shortcuts, no firewall rules, no Event Log (logs are files under the data dir). It returns to a clean machine two ways: (a) **explicit, immediate** — the app's "Remove" + "Also delete the index and logs" runs `uninstall --purge-data`, deleting service + task + the entire data dir at once (uninstall runs from the bundle exe, so the stable copy is not in use); `uninstall` without purge removes the service, task, **and the stable exe** (program clutter), keeping only the user's index/logs/`service.json`; (b) **automatic** — the weekly GC removes service + task + data, with the still-running stable exe and its now-empty dir scheduled for deletion on the next reboot (`MoveFileEx` delay-until-reboot, since a running image cannot delete itself). The per-user UI settings/logs at `%APPDATA%\find-my-files\` are the UI's, independent of the service.
- The `%ProgramData%` exe copy is a **security requirement**, not gratuitous footprint: the SCM launches it as LocalSystem, so it must live where a standard user cannot overwrite it (the user-writable portable folder would be a privilege-escalation vector now that start is unelevated). The privileged service is inherently machine-level; the portable / profile-clean ethos (ADR-0024 scope mode) applies to the *non-elevated* path.
- The service-object DACL and the stable-exe/data-dir non-writability are new security-relevant surfaces — recorded in docs/SECURITY.md (threats 9–10) and pinned by the `service_sddl` unit test (start/stop present; change-config/delete/write-DAC/write-owner absent), mirroring the pipe-SDDL pin.
- `idle_stop_secs` applies to the console `run` path too; `just service-dev` users who want it to stay up set `idle_stop_secs = 0`.

## Verification

- [x] `service_sddl` pin (Rust unit): user ACE has `RP`(start)/`WP`(stop), never `DC`/`SD`/`WD`/`WO`; converts to a real descriptor.
- [x] `idle_should_stop` / `gc_should_remove` truth tables + `last_use` round-trip (Rust unit).
- [x] `DecideAuto` Stopped→`StartThenPipe`, `WithoutService` fallback table (C# unit).
- [x] **Idle self-stop behavioural test, unelevated** (`tests/idle_stop.rs`): a console `run --no-index` with a 1 s idle timeout self-exits cleanly shortly after its only client disconnects.
- [ ] Elevated on-machine smoke (manual, recorded in SECURITY.md): `sc qc fmf-engine` shows `DEMAND_START`; after reboot the service is **not** running; app launch starts it **without UAC** and connects; closing the app self-stops it after the idle window; a `last_use` aged past the threshold makes `fmf-service gc` remove service + task + data.

## Re-examination triggers

- If cold start after a long absence (journal-wrapped full rescan) becomes a routine complaint, reconsider a low-footprint "freshness-only" mode (a lightweight USN-tail that keeps the snapshot current without serving), rather than reverting to a full resident server.
- If multi-user machines become a real target (already an ADR-0017 trigger), revisit who may start/stop the service (per-user vs. a group ACE).
