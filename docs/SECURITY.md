# Security — Threat Model and Defenses (v2 service split)

Current architecture: a privileged service `fmf-service` (LocalSystem, least privilege) reads NTFS $MFT/USN,
and the non-privileged UI connects over a named pipe. Decision history and rejected options are in
[ADR-0016](adr/0016-service-split-named-pipe.md) / [ADR-0017](adr/0017-service-security-model.md);
API spec verification is in [RESEARCH.md](RESEARCH.md).

## Threats and Defenses

| # | Threat | Defense |
|---|---|---|
| 1 | ACL-bypass name leak — the privileged indexer exposes file names invisible under the user's own ACL to **another user** | Restrict the pipe DACL to SYSTEM + the user SID (SID captured at install time **+ the everyday-user SID forwarded by the non-elevated UI via `--owner-sid`**. The latter is accepted only if it is a real-user type via `validate_user_sid` — keeps the everyday user from being locked out even under OTS elevation, while preventing injection of an arbitrary SID). No Authenticated Users / Everyone ACE (deny by default) + token check on connect |
| 2 | Remote connection | `PIPE_REJECT_REMOTE_CLIENTS` (+ server features are permanently out of scope per the won't-do list) |
| 3 | Anonymous connection | No anonymous ACE in the explicit DACL = deny by default (the NullSessionPipes default is policy-dependent, so do not rely on it) |
| 4 | Pipe-name squatting / spoofed server | Server: `FILE_FLAG_FIRST_PIPE_INSTANCE` **on the first instance only** (no flag on subsequent instances — name preemption is impossible as long as the first instance is held). Client: for the default pipe name, `GetNamedPipeServerProcessId` → **match against the SCM-registered fmf-engine service PID** (`QueryServiceStatusEx`; works non-elevated — a SYSTEM process token cannot be opened non-elevated [ACCESS_DENIED], and a session 0 process identity is not obtainable either. A squatter cannot register with the SCM [requires admin] so its PID will not match) |
| 5 | Malicious client input (malformed frame, huge len, unknown opcode, pathological regex) | 16 MiB length cap; validation failure drops the connection + `pipe_malformed_frames` counter. The whole dispatcher is a catch_unwind firewall (panic returns FMF_E_PANIC, the service survives). Regex is linear-time matching (no ReDoS) + compile caps `size_limit`/`dfa_size_limit`=1 MiB to gracefully reject computational DoS (overflow returns FMF_E_QUERY_SYNTAX. ADR-0023, RESEARCH.md) |
| 6 | Local DoS (connection flood, handle exhaustion, flush spamming) | Pipe instance cap 8 (overflow rejects the connection + `pipe_connections_rejected`). Result handle cap 64/connection (LRU evict → STALE). Flush is not exposed over the pipe (only the service-internal periodic flush and flush on stop). Events use a bounded queue + drop to protect the USN thread. Note that only the authorized same user can even reach this (#1) |
| 7 | Leak of the data file itself (.fmfidx contains every file name on every volume) | At install, apply a protective DACL to `%ProgramData%\find-my-files` (SYSTEM + Administrators; user read only on the logs subdirectory). Uninstall keeps data by default (shows guidance about leftovers); `--purge-data` deletes it |
| 8 | Residual risk (accepted) | An authorized user can search the "name/path" of files invisible under their own ACL (a structural property of name-only indexing; the contents and the actual ACL cannot be read). Targets single-user machines primarily; multi-user authorization is a re-examination trigger in ADR-0017 |
| 9 | Privilege escalation via the unelevated start/stop right (on-demand lifecycle, ADR-0027) | The *service-object* DACL grants the authorized user SID(s) only `SERVICE_START`/`SERVICE_STOP`/`SERVICE_QUERY_STATUS`+read (built by the unit-pinned `security::service_sddl`; never hand-rolled). It deliberately withholds `SERVICE_CHANGE_CONFIG`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER` from a standard user — granting any would let a non-admin repoint this **LocalSystem** service's binary and run code as SYSTEM. SYSTEM/Administrators keep full control (SCM management + the SYSTEM-run GC's `DeleteService`). The pin asserts start/stop are present and the four escalation rights are absent |
| 10 | Tampering with the stable SYSTEM binary (ADR-0027) | The on-demand service and the daily GC Scheduled Task run `%ProgramData%\find-my-files\fmf-service.exe` **as SYSTEM**; a standard-user-writable copy would be SYSTEM code execution. The binary is copied into the data root *after* it is hardened (threat 7: protected `D:P` SYSTEM+Administrators, object-inheriting), so the copy inherits a non-user-writable DACL. The GC task runs as `S-1-5-18` with `HighestAvailable` |

## Distribution Integrity (code signing)

Authenticode signing of the distributed binaries is done with SSL.com eSigner (individual IV). It is **active**: an IV
certificate (`CN=Yasunobu Sakashita`) was obtained 2026-06-24 and the eSigner Secrets are registered, so `release.yml`
signs the binaries from each tag. The wiring stays non-blocking (an unsigned build emits a `::warning::` rather than
failing if the Secrets are ever absent). The acquisition/renewal steps are in [SIGNING.md](SIGNING.md); the rationale
for the choice is in [ADR-0020](adr/0020-code-signing-provider.md). Signing is limited to the tag-driven `release.yml`
(the `ci.yml` dev artifacts are not signed).

## Manual Verification Checklist (run once before each release; record the result and date here)

Items that cannot be automated (require another user's token or another machine). The structure of the SDDL-building functions is pinned by unit tests.

- [ ] A pipe connection from another user (non-authorized SID) is rejected
- [ ] A remote connection to `\\<host>\pipe\fmf-engine-v2` is rejected
- [x] `%ProgramData%\find-my-files\index\*.fmfidx` cannot be read from a non-elevated process (2026-06-14: elevated on-machine verification **found a bug where it was readable with `Users:RX`** → fixed install → confirmed via icacls that both `index/` and `c.fmfidx` are SYSTEM + Administrators only. See the implementation record below)
- [x] `%ProgramData%\find-my-files\logs\engine.log` can be read from a non-elevated process (F12 diagnostics path) (2026-06-14: confirmed via icacls SYSTEM + Administrators + install-user read)
- [ ] After OTS elevation (elevated with a different admin account), the everyday user can still connect to the pipe non-elevated (`--owner-sid` propagation)
- [ ] "Re-register" to a running service → restart reflects `authorized_sids`, and a previously-rejected user can connect (`pipe client token rejected` stops)
- [ ] Leftovers after `fmf-service uninstall` match the guidance / are removed by `--purge-data`
- [ ] (ADR-0027) `sc qc fmf-engine` shows `DEMAND_START` (not AUTO_START); `sc sdshow fmf-engine` grants the user SID start/stop/query only — no `DC`/`SD`/`WD`/`WO`
- [ ] (ADR-0027) `%ProgramData%\find-my-files\fmf-service.exe` is **not** writable by a standard user (icacls: SYSTEM + Administrators only); a non-authorized user cannot start/stop the service
- [ ] (ADR-0027) App launch starts the stopped service **without a UAC prompt**; closing the app self-stops it after `idle_stop_secs`; `fmf-service gc` with an aged `last_use` removes service + Scheduled Task + data

Implementation record:

- **Code-level audit (2026-06-14)**: traced each defense in the table above to the implementation and confirmed it; conclusion was that the code paths are sound and covered by existing tests, no code change needed. See the commit for the full trace.
- **Real bug found and fixed in elevated on-machine verification (2026-06-14, threat 7)**: install did not explicitly apply the protective DACL to `index/` (while `logs/` was correctly applied explicitly), so the snapshot `index/c.fmfidx` (every file name on every volume) was **readable by all local users with `BUILTIN\Users:(RX)`**. Cause: `index/` inherits the Users ACE from `%ProgramData%` at creation time, and protecting the root afterward with `D:P` does not re-propagate to existing children because `SetFileSecurityW` (used by `set_dir_dacl`) does not (the asymmetry where `logs/` is correct and only `index/` is exposed is evidence of the root cause — reproduces even on a clean install). Fix: added `set_dir_dacl(&data_dir.join("index"), &data_dir_sddl())` to install in `fmf-service/src/main.rs` (the same explicit application as `logs/`). After rebuild + reinstall + icacls, confirmed both `index/` and `c.fmfidx` are SYSTEM + Administrators only; existing files were remediated with `icacls /reset`.
- **Runtime sign-off (remaining, not yet done)**: items requiring another user's token, a remote host, OTS elevation, or uninstall leftovers must be run in an elevated multi-user/network environment before release, with the date recorded.
