# ADR-0017: Service security model

Date: 2026-06-11 / Status: Accepted

## Decision

`fmf-service` runs as **LocalSystem** and, at install time, strips privileges to a minimal set via
`SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO` (SCM removes undeclared privileges from the token — docs/RESEARCH.md). The pipe has
4-layer defense — (1) explicit SDDL (SYSTEM + only the user SID captured at install time) (2) `PIPE_REJECT_REMOTE_CLIENTS`
(3) `FILE_FLAG_FIRST_PIPE_INSTANCE` (4) token check on connect (ImpersonateNamedPipeClient) —
guaranteeing "same user only, reject remote, reject anonymous". `%ProgramData%\find-my-files` gets a
protective DACL at install time (SYSTEM+Administrators; user read only on the logs subdirectory).
The standing threat-model document is docs/SECURITY.md (this ADR records the decision only).

## Rationale

- **Adopt LocalSystem / reject a dedicated low-privilege account + SeBackupPrivilege**: the verified fact only goes as far as
  "opening a volume handle (\\.\C:) requires administrator". There is no documented guarantee that SeBackupPrivilege grants raw
  volume reads (docs/RESEARCH.md — only describes ACL bypass for regular files). Rather than bet on an unverified privilege
  configuration, narrow the attack surface with the verified SYSTEM +
  privilege stripping + zero network capability + minimal pipe-surface opcodes.
- **Name the user SID / reject Authenticated Users**: Authenticated Users RW lets other users on a multi-user machine search
  every file name (a name leak that bypasses ACLs).
  **Allowing Administrators also fails**: in a UAC-filtered token the Administrators SID becomes
  SE_GROUP_USE_FOR_DENY_ONLY and is not used in allow ACEs (docs/RESEARCH.md). So store the install-running user's individual
  SID in service.json and use it in both the SDDL and the token check.
- **Handling OTS elevation (elevating with a different administrator account)**: when a standard user enters a different
  administrator credential at UAC, the install-running user (= the admin used to elevate) != the everyday user, and the everyday
  user can no longer connect to their own service. The non-elevated UI forwards its own SID to install via `--owner-sid`, and
  install validates it via `validate_user_sid` (accepting only SIDs for which LookupAccountSid returns SidTypeUser) before
  recording it alongside. The validation defends against threat 7 (injecting an arbitrary SID so someone else reads all file
  names) — install requires elevation and already has sc.exe-equivalent rights, but unresolvable / non-user-type SIDs are
  silently dropped (install itself does not fail).
- **Applying `authorized_sids` requires a restart**: the service reads service.json once at startup and bakes that value into
  both DACL construction and the connect-time token check (immutable while running). So to add a SID to a running instance,
  `install` (idempotent append) must be followed by `fmf-service restart` (stop->start) —
  `start` alone is a no-op with ERROR_SERVICE_ALREADY_RUNNING and keeps rejecting with the old allow list
  (the root cause of the regression that appeared as repeated `pipe client token rejected` on real hardware). The app's
  registration flow runs install->restart consecutively.
- **Reason for defense in depth**: a mistake building the SDDL string is the accident pattern of "silently wide open". Pin the
  structure of the build function with a non-elevated unit test, and place the connect-accept token check independently. Blocking
  anonymous access is primarily defended by the explicit DACL (no anonymous ACE = default deny) — do not rely on NullSessionPipes
  defaults, which are machine-type/policy-dependent (docs/RESEARCH.md).
- **Protective DACL on %ProgramData%**: under the default ACL a general user can directly read .fmfidx (which contains every file
  name) — no matter how hard the pipe is locked down, it leaks from the side. Leave user read only on logs (to keep the
  non-elevated F12 "copy diagnostic info" flow working).

## Consequences

- In addition to SCM registration, install atomically does SID capture -> service.json, the directory DACL, privilege stripping,
  and explicit `SERVICE_PRESHUTDOWN_INFO` (current Windows' default grace is only 10 seconds)
  -> not expressible via sc.exe, so the `fmf-service install` subcommand is the only choice (making the logic unit-testable).
- uninstall keeps data by default (`--purge-data` deletes .fmfidx/logs/service.json). The leftover artifacts are documented in
  README and SECURITY.md.
- **Client-connection prerequisites (verified on real hardware with the non-elevated UI)**: (1) the client opens the pipe at
  Identification level (C# `TokenImpersonationLevel.Identification` / Rust `SECURITY_SQOS_PRESENT`) — at the default
  anonymous level the server's `ImpersonateNamedPipeClient` gets an anonymous token, and the connect-time SID check rejects
  even authorized users entirely. (2) The client-side fake-server check (threat 4) is done not by SYSTEM-token comparison but by
  PID comparison of the SCM-registered service (`QueryServiceStatusEx`) — the non-elevated UI cannot open a SYSTEM process's
  token (ACCESS_DENIED) and cannot get the session-0 identity. Both were blind spots not exposed in console-mode tests where
  `authorized_sids` is empty and the token check is skipped; they only appear with the installed service.
- "reject other users" and "reject remote" cannot be auto-verified on the dev machine/CI (they need another user's token /
  another machine) -> substituted by structure-pinning the SDDL build function + the manual checklist in SECURITY.md. Do not
  create a pipe-creation code path that bypasses the build function (review point).
- Residual risk (accepted): an authorized user can also search the "name and path" of files invisible under their own ACL
  (a structural property of a file-name-only index; contents are unreadable). Documented in SECURITY.md.

## Re-examination triggers

- If a documented demonstration of a low-privilege indexer appears (e.g. a raw volume read means equivalent to
  FSCTL_READ_UNPRIVILEGED_USN_JOURNAL) -> re-evaluate demoting LocalSystem.
- SERVICE_SID_TYPE_RESTRICTED + an explicit ACE on the index directory (a v2.1 hardening candidate).
- Real demand for multi-user machines (UX for registering multiple authorized SIDs).
