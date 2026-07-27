# Security — Threat Model and Defenses (v2 service split)

Current architecture: a privileged service `fmf-service` (LocalSystem, least privilege) reads NTFS $MFT/USN,
and the non-privileged UI connects over a named pipe. Decision history and rejected options are in
[ADR-0016](adr/0016-service-split-named-pipe.md) / [ADR-0017](adr/0017-service-security-model.md);
API spec verification is in [RESEARCH.md](RESEARCH.md).

## Threats and Defenses

| # | Threat | Defense |
|---|---|---|
| 1 | ACL-bypass name leak — the privileged indexer exposes file names invisible under the user's own ACL to **another user** | Restrict the pipe DACL to SYSTEM + the user SID (SID captured at install time **+ the everyday-user SID forwarded by the non-elevated UI via `--owner-sid`**. The latter is accepted only if it is a real-user type via `validate_user_sid` — keeps the everyday user from being locked out even under OTS elevation, while preventing injection of an arbitrary SID). No Authenticated Users / Everyone ACE (deny by default) + token check on connect. SID mismatch disconnects only that client. A token-verification **API failure** fails closed and stays recoverable: the accept loop drops the listener, `serve()` observes the lost listener and stops the service gracefully (flush → SCM `Stopped`), and the next on-demand start rebuilds it. No client is ever admitted unverified, and a Running service with no listener — unrecoverable, since clients reconnect forever and `start` is a no-op — is the state this explicitly excludes. `RevertToSelf` failure still aborts the process outright, so no later privileged work can continue under a client token |
| 2 | Remote connection | `PIPE_REJECT_REMOTE_CLIENTS` (+ server features are permanently out of scope per the won't-do list) |
| 3 | Anonymous connection | No anonymous ACE in the explicit DACL = deny by default (the NullSessionPipes default is policy-dependent, so do not rely on it) |
| 4 | Pipe-name squatting / spoofed server | Server: `FILE_FLAG_FIRST_PIPE_INSTANCE` **on the first instance only** (no flag on subsequent instances — name preemption is impossible as long as the first instance is held). Client: for the default pipe name, `GetNamedPipeServerProcessId` → **match against the SCM-registered fmf-engine service PID** (`QueryServiceStatusEx`; works non-elevated — a SYSTEM process token cannot be opened non-elevated [ACCESS_DENIED], and a session 0 process identity is not obtainable either. A squatter cannot register with the SCM [requires admin] so its PID will not match) |
| 5 | Malicious client input (malformed frame, huge len, unknown opcode, pathological query) | Every allocation-bearing bound is contract, not a tunable: the caps live in `fmf-contract::limits` and are enforced identically at the C#, FFI, pipe, and core boundaries. The fixed 16-byte header is checked against the **opcode-specific** cap **before any payload is allocated or read**, so an attacker-declared length can never reserve memory (the separate global cap exists for response frames, not as a request-allocation allowance). Validation failure drops the connection + `pipe_malformed_frames` counter. Query text, parsed groups, parsed terms, and regex terms are each separately capped, so neither a long query nor a deeply combinatorial one can force unbounded parse work. IndexStart is transactionally restricted to unique canonical labels in the current fixed-NTFS set, so it cannot launch workers for FAT/exFAT, removable, network, or synthetic paths. The whole dispatcher is a catch_unwind firewall (panic returns FMF_E_PANIC, the service survives). Regex is linear-time matching (no ReDoS) + compile caps `size_limit`/`dfa_size_limit`=1 MiB to gracefully reject computational DoS (overflow returns FMF_E_QUERY_SYNTAX. ADR-0023/ADR-0044, RESEARCH.md) |
| 6 | Local DoS (connection flood, handle exhaustion, flush spamming) | Every unbounded-growth path is given a fixed compile-time ceiling rather than a runtime policy: concurrent pipe instances (overflow rejects the connection + `pipe_connections_rejected`), decoded requests in flight per connection (bounded backpressure; stop/disconnect cancels a blocked producer), result handles per connection (LRU evict → STALE, so exhaustion degrades into the existing re-query path instead of failing), and the per-connection event queue. The contract-visible ceilings are `fmf-contract::limits`; the accept-loop instance cap is the pipe server's own constant. Flush is not exposed over the pipe (only the service-internal periodic flush and flush on stop). Events use a bounded queue + drop to protect the USN thread. Note that only the authorized same user can even reach this (#1) |
| 7 | Leak/tamper of the machine index (.fmfidx contains every file name) | Install creates the root protected, publishes it by exact handle, and pins its NTFS identity in protected HKLM; an unproven fixed-name object is quarantined, never repaired in place. Root/descendant mutation is handle-bound and atomic. Administrators own the protected SYSTEM+Administrators tree; only logs add authorized-user read. Fixed paths reject reparse points at install, service start, GC, and purge. The existing tree is hardened **bottom-up and write-nothing-on-the-way-down**: a directory receives its descriptor only after its whole subtree has been opened `FILE_FLAG_OPEN_REPARSE_POINT`, type-checked, and given a protected descriptor of its own, so **no object's access depends on what it inherits** and an inheritable ACE never reaches a level not yet proven free of reparse points. Both halves are measured rather than assumed: an unelevated test plants a real junction (which, unlike a symlink, any standard user can create) and pins that the target's descriptor is untouched and that the walk refuses the tree; another builds the production shape — a child directory holding only files — which is the shape no earlier test built, because they all created an empty root and so never executed the walk at all |
| 8 | Residual risk (accepted) | An authorized user can search the "name/path" of files invisible under their own ACL (a structural property of name-only indexing; the contents and the actual ACL cannot be read). Targets single-user machines primarily; multi-user authorization is a re-examination trigger in ADR-0017 |
| 9 | Privilege escalation via the unelevated start/stop right (on-demand lifecycle, ADR-0027) | The *service-object* DACL grants the authorized user SID(s) only `SERVICE_START`/`SERVICE_STOP`/`SERVICE_QUERY_STATUS`+read (built by the unit-pinned `security::service_sddl`; never hand-rolled). It deliberately withholds `SERVICE_CHANGE_CONFIG`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER` from a standard user — granting any would let a non-admin repoint this **LocalSystem** service's binary and run code as SYSTEM. SYSTEM/Administrators keep full control (SCM management + the SYSTEM-run GC's `DeleteService`). The pin asserts start/stop are present and the four escalation rights are absent |
| 10 | Tampering with the stable SYSTEM binary / elevated helper or adjacent-DLL hijack ([ADR-0045](adr/0045-elevated-service-dependent-load-policy.md)) | Before the UI passes the bundled helper to `runas`, it locks the image without write/delete sharing, locks each non-root parent directory the same way **wherever this token is granted `DELETE` on it** (a directory whose DACL withholds `DELETE` from the app's token — `C:\ProgramData` does, for a standard user — denies an attacker holding that same token the rename or delete just as strictly, so the open degrades to observation instead of failing closed), rejects file or directory reparse points, compares a constant-time SHA-256 over Windows' Authenticode PE digest stream with the exact digest embedded by `xtask publish`, and requires `WinVerifyTrust` to accept the Authenticode signature. The digest excludes only the mutable certificate table, so it is identical before/after release signing and rejects an older same-signer helper as well as an unsigned replacement. The service statically links the MSVC CRT and embeds `/DEPENDENTLOADFLAG:0x800`, limiting all remaining static imports to System32; both publish and package parse the PE Load Config and require that exact value. Thus locking the EXE cannot be bypassed with a planted sibling DLL, and no VC Redistributable is a hidden prerequisite. Unpinned or incorrectly linked developer builds cannot cross this elevation boundary. The installed service/GC then run `%ProgramData%\find-my-files\fmf-service.exe` as SYSTEM; install copies it only after threat-7 DACL hardening and re-hardens it. The GC task runs as `S-1-5-18` with `HighestAvailable`. Install invokes the Known-Folder-resolved absolute `System32\schtasks.exe` (never PATH search) and XML-escapes its action path |

## Required machine-security gate

`just test-admin` pins and runs
`pipe::admin_security_tests::named_pipe_security_boundaries_are_enforced_on_real_tokens_and_transports`.
It must prove all three boundaries on real Windows tokens and transports:

1. a temporary local standard user's distinct `TokenUser` SID is denied by the
   production pipe DACL, while the authorized user connects to the same pending
   pipe instance;
2. a deliberately wide test-only DACL admits that other user at the kernel, but
   server-side `verify_client` rejects and disconnects it; and
3. `\\COMPUTERNAME\pipe\...` succeeds with `PIPE_ACCEPT_REMOTE_CLIENTS`, then
   fails with `PIPE_REJECT_REMOTE_CLIENTS` under the same identity and DACL.

The remote control is fail-closed: inability to prove remote transport fails the
test. The test account and password are unique per process, the password is never
logged and is cleared after logon, and RAII deletes the account during normal
completion or panic unwinding. Release and nightly admin workflows upload this
test in `build/engine/nextest/admin/admin.xml`; removing or renaming it makes the
`test-admin` inventory preflight fail before execution.

## Distribution integrity

`release.yml` isolates build, signing, and publishing into separate jobs. Only the
approval-gated signing job receives SSL.com eSigner secrets; every first-party PE
listed in the committed signing manifest must pass Authenticode chain, RFC 3161
timestamp, and signer-subject verification.
The unsigned service's Authenticode-stable image digest is embedded into the app
before signing; collection proves that signing preserved every first-party PE
image, and package independently rechecks the service load policy before sealing
the ZIP. Runtime verifies the same service identity immediately before UAC.
Actions are commit-SHA pinned and publishing fails closed. Operational steps live
in [RELEASING.md](RELEASING.md); rationale is in
[ADR-0020](adr/0020-code-signing-provider.md) and
[ADR-0029](adr/0029-ci-signing-cka-pipeline.md).
