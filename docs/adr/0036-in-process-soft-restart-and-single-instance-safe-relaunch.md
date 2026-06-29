# ADR-0036: In-process soft restart for engine changes; single-instance-safe relaunch for language

Date: 2026-06-29 / Status: Accepted (fixes the [#107](https://github.com/P4suta/find-my-files/pull/107) onboarding relaunch, which collided with [ADR-0030](0030-tray-resident-mode.md) single-instancing; no service / transport / contract change)

## Context

The engine transport is chosen once when the page is built (`EngineClientFactory.Resolve`), so after a first-time service registration the running instance is still on the empty fake engine. #107 made onboarding "take effect" by relaunching the process with `--engine=pipe` (`ShellOps.RelaunchWith`: `Process.Start(self)` then `Application.Current.Exit()`).

That relaunch is **structurally incompatible with single-instancing** (ADR-0030, `Program.DecideRedirection`, keyed on the fixed string `"find-my-files"`):

1. The relaunched process starts while the original is still alive, so `AppInstance.FindOrRegisterForKey` finds the original as primary → `RedirectActivationTo` + `return 0`. `Application.Start`/`OnLaunched` never run and `--engine=pipe` is silently dropped.
2. The original receives `OnActivated` → `ShowFromTray` (still the empty fake) and then runs the queued `Application.Current.Exit()`.
3. **Both processes are gone — the app disappears.** The user reopens it manually; that fresh launch (no primary) auto-detects the now-running service and connects, masking the bug as "I had to start it twice."

Confirmed on a real bundle: after `setup → Ok` there is **no** `--engine=pipe` launch line, and the next launch logs `engine: pipe (…, probe succeeded)` (the *auto* path, line 127 — a manual restart), not the `--engine=pipe` direct path (line 80). Every relaunch caller had the same latent bug: scope apply, the service-manager register/restart, the uninstall-while-on-pipe recovery, and the language switch.

The #107 tests injected a fake relaunch `Action`, so the real process × `AppInstance` interaction was never exercised — the "state-transition blind spot" pattern again.

## Decision

Split relaunch by the **reason** it exists; spawning a process to mutate in-memory transport state is the anti-pattern.

1. **Engine re-resolution → in-process soft restart (`App.SoftRestart` / `App.SoftRestartIntoPipe`, `AppReload`).** No new process: close the diagnostics window, re-resolve `App.EngineClient` (the same resolve-or-fallback the launch path uses), re-navigate the root `Frame` to a fresh `MainPage` (which rebuilds its `MainViewModel` against the new engine), then dispose the old engine. The window, tray, and process stay alive. Used by onboarding (`EnableSearchAsync`), scope apply (`ApplyScopeChange`), the service-manager register/restart, and the uninstall recovery.
2. **Language change → true restart via `AppInstance.Restart` (`IAppRestart` / `RealAppRestart`).** `PrimaryLanguageOverride` is a process-global WinRT setting applied in the `App` ctor, and the `Loc` `ResourceLoader` and `MainWindow` chrome are built once — only a fresh process re-localizes the whole shell. `AppInstance.Restart` fully terminates this process *before* the new one registers, so single-instancing lets the new instance become primary instead of redirecting back to the dying one. A non-success return is surfaced (notify, don't go silent).

`AppReload` is pure over its boundaries (resolve / get-set engine / re-navigate / close-diagnostics) so the load-bearing ordering and once-only disposal are unit-tested without a real Frame or window.

## Rationale

- The soft restart is what ADR-0030 already argued for: the pain is the WinUI/.NET cold start, and a tray-resident app exists precisely to *keep the process hot*. Killing and respawning the process to flip an in-memory field contradicts that; re-resolving in place does not.
- x:Bind `OneTime` on `IsDisconnected` / `IsReady` / `IsScopeMode` stays correct **because** the soft restart builds a *fresh* page and view model — the properties are re-evaluated against the new engine, exactly as a process relaunch used to give for free.
- The "ItemsSource must not be swapped / `VirtualResultList` is page-lifetime" UI rules are about a *live* page; a fresh page is the sanctioned reset, so they are not violated.
- Language genuinely needs a new process, and `AppInstance.Restart` is the purpose-built, single-instance-aware API for it — not a raw `Process.Start` + `Exit`, which is the very thing that broke.

## Trade-off

A small residual race exists for the language restart: between `AppInstance.Restart` terminating this process and the fresh one registering the key, a third manual launch could momentarily become primary. It is rare and self-heals on the next launch. The in-process soft restart has no such window (no second process).

## Rejected alternatives

- **Fix only the single-instance side — make the spawned `--engine=pipe` process win (e.g. `AppInstance.GetCurrent().UnregisterKey()` before spawn) for every relaunch.** One mechanism, but it re-pays the full WinUI/.NET cold start ADR-0030 fights, tears the connection state machine down across a process boundary, drops the tray, and flashes the window. `UnregisterKey` is also `[Experimental]` in the SDK (lint friction). Kept as the documented fallback for the language path *only* if `AppInstance.Restart` proves unreliable for this unpackaged self-contained app on the pinned SDK.
- **In-process soft restart for language too.** Rejected: a page rebuild updates the page body but not the `MainWindow` title bar / tray tooltip (resolved once at window construction) nor the process-global `ResourceLoader`, so the shell would be half-translated.
- **An in-place engine swap on the existing page (no page rebuild).** `MainViewModel` deeply embeds the engine (event marshaler, search orchestrator, perf panel) behind a `readonly` field and `OneTime` bindings; a swap means rebuilding most of it — the page rebuild *is* the clean form of that.

## Consequences

- **No wire-contract / golden / ABI change.** Pure C# app layer.
- `ShellOps.RelaunchIntoPipe` and the `Process.Start`+`IAppExit` `RelaunchWith` are removed; `IAppExit`/`DispatcherAppExit` go with them. `ShellOps.Relaunch` now means "true restart, language only" and goes through `IAppRestart`. `IProcessRunner` stays (it still backs `ShellOps.Open`).
- The injected relaunch seams (`ServiceProvisioner._relaunch`, `MainViewModel._relaunch`) are unchanged; only their production defaults move to `App.SoftRestart*`, so the action-injection tests stay green.
- `MainPage` disposes its view model on `Unloaded` (the `Frame` does not), so a soft restart releases the old engine-event subscriptions; the disposal is idempotent with the `Window.Closed` engine dispose.
- **Testability**: `AppReload` (ordering + once-only dispose + re-entry guard) and the `IAppRestart` seam (empty-arg restart + swallowed-failure) are unit-tested. `App` / `Program` / `MainWindow` / `MainPage` stay `[ExcludeFromCodeCoverage]` view-shell (ADR-0022); the redirect/navigation itself is covered by `docs/MANUAL_SMOKE.md` + a real run.
- **Security**: unchanged — all local to the unelevated app; the privileged service surface (docs/SECURITY.md) is untouched.

## Verification

- [x] `AppReloadTests`: closeDiagnostics → resolve → set → re-navigate → dispose-old order; old engine disposed exactly once and after the swap; a re-entrant `Run` during the rebuild is ignored.
- [x] `ShellOpsTests`: `RelaunchWith` restarts with empty arguments; a failed restart is swallowed (notify, don't crash).
- [x] Existing action-injection suites stay green (`MainViewModelConnectionTests`, `ScopeSetupTests`, `ServiceProvisionerTests`, `UiThreadAffinityTests`).
- [ ] **Manual smoke (one elevated UAC)**: from a clean state (`sc query fmf-service` = not installed), onboarding one-click → the search box appears in the **same** window with no process flash; `app.log` shows **no** `--engine=pipe` launch line (proof it stayed in-process). Scope apply re-walks in-process; service-manager register/restart and uninstall recovery each transition without the app vanishing.
- [ ] **Manual smoke**: language switch restarts and the whole shell (incl. title bar) changes language; a second launch still redirects to the one instance.

## Re-examination triggers

- If `AppInstance.Restart` proves unreliable for the unpackaged self-contained bundle on the pinned WindowsAppSDK, switch the language path to the `UnregisterKey`-before-spawn fallback (with a justified experimental-API suppression).
- If a future feature needs to change transport *without* losing live page state (e.g. results), revisit the in-place engine swap rejected above.
