# ADR-0030: Tray-resident mode (UI process stays, hot-held engine)

Date: 2026-06-25 / Status: Accepted (an app-side, opt-in lifecycle layered on the [ADR-0027](0027-on-demand-service-lifecycle.md) on-demand service; the service, transport, and contract are unchanged)

## Decision

Add an opt-in **tray-resident mode**. A user-scope setting `close_to_tray` (default **off**) gates it. When on:

1. **Close (×) hides to the tray** — the main window's `AppWindow.Closing` is cancelled and the window is `AppWindow.Hide()`-den (this also removes the taskbar button). The only real exit is the tray icon's right-click **Exit**.
2. **Always hot** — while tray-resident the process keeps the `MainWindow`, its `MainViewModel`, *and* the live engine connection alive. Restore is `AppWindow.Show()` + `Activate()` — nothing is rebuilt, so the query text, results and scroll position survive, and the first search after restore is zero-latency.
3. **Self-written tray icon** — `Shell_NotifyIcon` via `[LibraryImport]` (no third-party package). Its callback message and `WM_TASKBARCREATED` are received by **subclassing the `MainWindow` HWND** with `SetWindowSubclass`. The context menu is a Win32 `TrackPopupMenuEx`.
4. **Single-instance** — `DISABLE_XAML_GENERATED_MAIN` + a hand-written `Program.cs` using `AppInstance.FindOrRegisterForKey` + `Activated` redirection, so a second launch (e.g. from the Start menu while tray-resident) restores the first instance instead of spawning a duplicate icon. (This redirection later collided with #107's process relaunch — an in-app relaunch redirected back to the dying original and took the app down; [ADR-0036](0036-in-process-soft-restart-and-single-instance-safe-relaunch.md) resolves it by re-resolving the engine in-process instead of relaunching.)
5. **Engine/service (Rust) and the wire contract are untouched.** Everything lives in the C# app layer.

## Rationale

- The owner's symptom is "re-opening the app makes it heave itself up." The cause is the **WinUI/.NET UI-process start cost**, not the service: ADR-0027 already keeps the service hot for `idle_stop_secs` (300 s) after the last client drops and restores in ≤2 s. The lever that actually helps is keeping the *UI process* alive — which is exactly what a tray resident is.
- **Always-hot is a deliberate owner choice.** Keeping the engine connection up while hidden means the service's idle self-stop (`idle_should_stop = … && active==0 && …`, `fmf-service/src/lifecycle.rs`) is intentionally held off — a live pipe is never `active==0`. That is the *correct* consequence of "always hot": the index stays fresh via the service's USN tracking and the first search never pays a cold start. ADR-0027 chose "minimal footprint over an always-hot index" for the *default* lifecycle; this is the opposite preference, scoped to *opt-in tray mode only* and driven from the UI without touching the service.
- **Hiding, not disconnecting, is what makes restore instant and stateful.** Because nothing is torn down, restore needs no `EngineClientFactory.Resolve`, no window rebuild, and no UI-state save/restore. This also collapses a large amount of would-be machinery (connection suspend/resume, window re-creation) that a "drop the connection while hidden" design would require.
- The real exit path (tray **Exit**) still runs the existing `Window.Closed` teardown — `EngineClient.Dispose()` drops the pipe, `active` falls to 0, and the service returns to its normal ADR-0027 idle self-stop. So tray mode changes *when* we disconnect, never *how*.

## Trade-off

While tray-resident, both the UI process and the service (the index — ~110 B/file, ≈100 MB at 1M files) stay in RAM. That is the cost of "always hot," accepted knowingly. It is bounded by being **opt-in and default-off**: a user who leaves `close_to_tray` off gets the unchanged ADR-0027 on-demand behaviour (close → service idle-stops after 5 min → zero RAM). Turning tray mode off at any time returns to that footprint.

## Rejected alternatives

- **Drop the connection while hidden, let idle-stop reclaim the index** — lighter (the service falls away after 5 min and the existing idle window doubles as a hot grace period), but the first search after a >5 min absence pays a cold start. The owner chose always-hot. Kept as a documented re-examination trigger; the `WindowSubclass` plumbing supports adding it later as a second setting with no structural change.
- **A message-only window (`HWND_MESSAGE`) for the tray callback** — `WM_TASKBARCREATED` is a broadcast, and broadcasts do not reach message-only windows, so the icon would never recover after an Explorer restart. The top-level `MainWindow` does receive it.
- **`SetWindowLongPtr(GWLP_WNDPROC)` to replace the window proc** — clobbers the `DesktopWindowXamlSource` proc that WinUI's top-level window relies on. `SetWindowSubclass` chains instead, preserving XAML's handling.
- **A NotifyIcon NuGet (e.g. H.NotifyIcon.WinUI)** — against the codebase's minimal-dependency posture (the app references only WindowsAppSDK + CommunityToolkit.Mvvm). The Win32 surface is small and matches the existing self-written P/Invoke seams (`ServiceSetup`, `IRevealApi`, `ShellOps`).
- **Global hotkey launcher** — declined by the owner for now. The `WindowSubclass` base is the natural host for a future `RegisterHotKey`/`WM_HOTKEY`, so nothing forecloses it.
- **Minimize-to-tray** — folded into "× hides to tray" per the owner's choice; one gesture, not two.

## Consequences

- **No wire-contract / golden / ABI change.** One additive `AppSettings` field (`close_to_tray`), picked up automatically by the source-generated `AppSettingsJsonContext` as snake_case JSON — mirrors ADR-0027's additive-`service.json` posture.
- **The process entry point changes**: `DISABLE_XAML_GENERATED_MAIN` plus a hand-written `Program.cs`. The `App` ctor order (`ApplyLanguageOverride → InitializeComponent → ExceptionPolicy.Install`) is preserved unchanged; `Program.Main` only wraps `Application.Start`.
- **New Win32 interop surface** (`Shell_NotifyIcon`, `SetWindowSubclass`, `TrackPopupMenuEx`), pinned to System32 like every existing import. The `SUBCLASSPROC` delegate, the HICON, and the tray identity are held in fields for the process lifetime (CLAUDE.md "FFI-callback delegates are field-held" — GC reclaim would dangle the native pointer). `OnActivated` (single-instance redirect) fires on a background thread, so it marshals to the UI thread via the cached `App.DispatcherQueue` before any window work.
- **Testability**: the view-shell pieces (`Program`, `TrayIcon`, `WindowSubclass`, `TrayMenu`) are `[ExcludeFromCodeCoverage]` like the other window shells (ADR-0022). The close-vs-hide decision is extracted into a pure `WindowLifecycle` function and table-tested.
- **Security**: single-instance keying and the tray HWND are local to the unelevated app; no change to the privileged service surface (docs/SECURITY.md unaffected).

## Verification

- [ ] `WindowLifecycle` close-vs-hide truth table (C# unit): normal × with `CloseToTray` on/off × explicit-exit on/off.
- [ ] `AppSettings` `close_to_tray` round-trip (C# unit): default false → save → load.
- [ ] UI automation: the gear-menu toggle persists; × hides the window; restore shows it; tray **Exit** ends the process.
- [ ] Manual smoke: with the toggle on, × hides to tray (taskbar button gone); left-click restores with query/scroll state intact; right-click **Exit** fully exits; a second launch while tray-resident restores instead of duplicating; after an Explorer restart the icon reappears (`WM_TASKBARCREATED`); while tray-resident the service stays alive (F12 diagnostics / Task Manager) and self-stops ~5 min after a real exit.
- [ ] Regression: with `close_to_tray` off (default), launch/close behaviour is byte-for-byte the current on-demand flow.

## Re-examination triggers

- If the always-hot resident footprint becomes a complaint, add a second mode that drops the connection while hidden (cold start on restore) — the `WindowSubclass`/`AppWindow.Hide` plumbing is unchanged; only the hide/show handlers gain a Dispose/Resolve pair.
- If a global hotkey is requested, host `RegisterHotKey`/`WM_HOTKEY` on the existing `WindowSubclass` (no new top-level window or message pump needed).
- If multi-instance (e.g. per-volume windows) ever becomes a goal, revisit the single-instance key (it would move from a fixed string to a per-window key).
