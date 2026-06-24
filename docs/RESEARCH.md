# Verified technical facts (researched 2026-06-10, primary sources confirmed)

Design decisions assume this file. Sources at the end of each item.

## NTFS / MFT / USN journal

- **FSCTL_ENUM_USN_DATA** (DeviceIoControl, winioctl.h, documented) is the official API to enumerate MFT records. Call it repeatedly with `MFT_ENUM_DATA_V0/V1` as input, starting from `StartFileReferenceNumber=0`. The returned `USN_RECORD_V2` has FRN, parent FRN, file name, and FileAttributes, but **no file size or timestamp** (TimeStamp is the journal-record time). Indexing with size and date requires reading the raw $MFT ($STANDARD_INFORMATION/$FILE_NAME/$DATA) or an extra per-file query.
  https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data
  https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2
- **Incremental monitoring**: `FSCTL_QUERY_USN_JOURNAL` to get UsnJournalID/NextUsn → `FSCTL_READ_USN_JOURNAL` (`READ_USN_JOURNAL_DATA_V0`, blocking subscription possible with `BytesToWaitFor>0`). The state to persist is the **UsnJournalID + last-processed USN** pair. The journal is maintained by the OS, so changes made while the app is stopped can be caught up.
  https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal
- **Error fallback (standard pattern)**: `ERROR_JOURNAL_NOT_ACTIVE` → create with `FSCTL_CREATE_USN_JOURNAL` (admin required). `ERROR_JOURNAL_DELETE_IN_PROGRESS` (deletion continues across reboots). Saved USN older than FirstUsn → `ERROR_JOURNAL_ENTRY_DELETED`. These plus a JournalID mismatch **fall back to a full rescan**.
  https://learn.microsoft.com/en-us/windows/win32/fileio/creating-modifying-and-deleting-a-change-journal
- **FRN→path**: USN records have no path string. Hold an FRN→(name, parent FRN) map for all directories and build paths lazily by walking the parent chain up to the root (fixed at MFT record 5 on NTFS). A folder rename/move updates only that one record; no records are emitted for its children. FRN is 64-bit on NTFS (low 48 bits = record number + high 16 bits = sequence). ReFS is 128-bit (USN_RECORD_V3) — out of scope for MVP but accounted for in the ID type design.
- **Privileges**: Opening a volume handle (`\\.\C:`) requires admin (CreateFile official Remarks: "The caller must have administrative privileges"). The undocumented `FSCTL_READ_UNPRIVILEGED_USN_JOURNAL` allows non-elevated journal reads, but it is undocumented and has no ENUM equivalent, so the initial scan requires elevation.
  https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew
- **Hard links**: Multiple $FILE_NAME attributes within a single MFT record. A USN record's file name is normally only the "first link name". → MVP uses "one representative name per FRN".
- **Symbolic links / junctions**: not followed (cycle-matching cost). Index the reparse point itself as a single entry.

## Search syntax (real-usage research)

- Real-usage research (HN etc.) centers on substring default, `space`=AND, `|`=OR, `!`=NOT, `""`=phrase, `*?` (whole-filename match), `ext:` `path:` `size:` `dm:` (ranges `a..b` `>x`). `regex:`/`content:` are niche, and content search is inherently slow. → Supports the syntax scope and the "filename-only indexing" tradeoff (ADR-0001).

## Competitors / prior art (as of 2026-06)

- "Rust engine + native WinUI 3 + truly FOSS" is an empty niche. The strongest competitor, omni-search (Eul45, started 2026-02, 517 stars, MIT), is Tauri v2 + React + C++, requireAdministrator approach.
- Past FOSS clones are all stalled: Orange (Rust/Tauri/Tantivy, walk-based without MFT, stopped 2023-10), FastFileSearch (2016), Indexer++ (2019), SwiftSearch (actually CC BY-NC = non-FOSS, 2019).

## Real C: name/size statistics (2026-06-11, `fmf stats C: --name-stats`, 1,268,450 entries)

Primary data for layout decisions and synthetic-benchmark calibration (re-measure with this command):

- fold-identical (lower==orig) = 73.2% / unique names 53.2% / unique after fold 53.0%
- name length (WTF-8 bytes): mean 29.7 / p50 18 / p90 90 / p99 110 / max 171
- files over 4GiB = 10 (0.0008%)

See `docs/adr/` for design and rejection decisions and their numeric rationale.

## Rust crates (existence and maturity confirmed)

- `ntfs-reader` 0.4.5 (MIT/Apache-2.0, updated 2026-03): full raw-$MFT record scan (README benchmark: Vec Cache 3.756s / HashMap 4.981s / No Cache 12.3s, environment not stated). FileInfo gives name/path/size/created/modified. **Cannot retrieve all hard-link names (one representative name)**.
- `usn-journal-rs` (wangfu91, MIT, updated 2026-05): MFT enumeration + USN monitoring + FRN path resolution. Read as a reference implementation (policy: do not depend on it).
- `windows-sys` 0.61: complete FSCTL constants, MFT_ENUM_DATA, USN_RECORD, etc. The USN wrapper is implemented in-house (~200 lines).
- `memchr` (memmem::Finder = SIMD substring), `rayon`, `parking_lot`, `thiserror`, `tracing`, `xxhash-rust`.

## WinUI 3 (Windows App SDK)

- **Data virtualization**: random access with a known count uses **non-generic `IList` + `INotifyCollectionChanged` + `IItemsRangeInfo` + placeholders**. Explicitly supported in current WASDK (MS Learn updated 2026-03). `IList<T>` alone does not work (#1809). `ISupportIncrementalLoading` has crash reports (#6883), avoid it. ItemsView/ItemsRepeater support neither interface. Setting ItemsPanel to anything other than ItemsStackPanel disables virtualization.
  https://learn.microsoft.com/en-us/windows/apps/develop/performance/listview-and-gridview-data-optimization
- **Tray / hotkey**: no native support. H.NotifyIcon.WinUI + in-house RegisterHotKey + an HWND_MESSAGE hidden window (WM_HOTKEY).
- **DPI**: the WinUI 3 template defaults to Per-Monitor V2.
- **MSIX × requireAdministrator is a poor fit** (allowElevation etc. constraints, almost always rejected in Store review) → unpackaged + self-contained distribution.
- **Known constraints of elevated processes**: D&D from Explorer is not possible (UIPI). ShellExecute directly from an elevated process launches the associated app elevated too → de-elevate via `explorer.exe "<path>"` (standard pattern).
- WASDK 1.6+ supports Native AOT (official sample cuts startup by about 50%). However, the "instant launch" experience is best ensured by a resident tray + hotkey.

## Security — v2 service separation (researched 2026-06-11, primary sources confirmed)

A privileged-indexer → non-privileged-UI design carries an information-disclosure risk: exposing file names and paths that should be invisible per ACL. The v2 threat model and defenses are in `docs/SECURITY.md`; decision records are ADR-0016/0017. Below is the supporting research:

- **PIPE_REJECT_REMOTE_CLIENTS** (CreateNamedPipeW dwPipeMode): officially stated as "Connections from remote clients are automatically rejected". Direct mechanism for remote rejection.
  https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew
- **FILE_FLAG_FIRST_PIPE_INSTANCE**: creating a second instance fails with ERROR_ACCESS_DENIED (officially stated). Defends against pipe-name squatting. Same source as above.
- **GetNamedPipeServerProcessId**: a client can get the server process PID (fake-server detection: PID → verify the token is SYSTEM).
  https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeserverprocessid
- **Anonymous access (caution)**: the default for anonymous restriction via NullSessionPipes is **machine-type/policy dependent** (enabled on DC/standalone, Not defined on member/client). Make an explicit DACL (no anonymous ACE = default deny) the primary defense for blocking anonymous access.
  https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/network-access-restrict-anonymous-access-to-named-pipes-and-shares
- **Deny-only Administrators in a UAC-filtered token**: in a non-elevated process the BUILTIN\Administrators SID becomes SE_GROUP_USE_FOR_DENY_ONLY and is **not used for allow ACEs** (only for deny-ACE matching). A pipe DACL that "allows Administrators" cannot be connected to by a non-elevated UI → naming the user's individual SID is mandatory.
  https://learn.microsoft.com/en-us/windows/win32/secauthz/sid-attributes-in-an-access-token
- **ImpersonateNamedPipeClient**: the server can obtain and inspect the client's token (SID matching at connect time = defense in depth against a misconfigured DACL).
  https://learn.microsoft.com/en-us/windows/win32/ipc/impersonating-a-named-pipe-client
- **SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO** (ChangeServiceConfig2): declaring required privileges makes the SCM strip undeclared privileges from the process token at startup (SeChangeNotifyPrivilege always remains; for shared-process services the union applies). Used to disarm LocalSystem.
  https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_required_privileges_infow
- **SERVICE_CONTROL_PRESHUTDOWN (caution)**: the default grace period is **10 seconds on Windows 10 1703 and later** (3 minutes before that). Saving a large snapshot requires explicitly extending it via `SERVICE_PRESHUTDOWN_INFO` (dwPreshutdownTimeout).
  https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_preshutdown_info
- **windows-service crate** (Mullvad, v0.8.1 2026-05, MIT/Apache-2.0): provides define_windows_service! and service_control_handler::register. A PRESHUTDOWN handler can be registered.
  https://github.com/mullvad/windows-service-rs
- **SeBackupPrivilege and raw-volume reads**: what is documented goes only as far as "retrieving content of normal files by bypassing the ACL". There is **no documented guarantee** that a raw volume handle to \\.\C: can be opened with SeBackupPrivilege alone (research scope: Managing Privileges in a File System and others). Volume handles require admin (see "Privileges" item above) → the basis on which ADR-0017 rejected the dedicated low-privilege-account proposal.
  https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/privileges

## On-demand service lifecycle (researched 2026-06-23, premise = ADR-0027)

The v2 service was registered SERVICE_AUTO_START (boot-resident). ADR-0027 moves it to demand-start + unelevated start/stop + idle stop + a daily GC. Supporting facts:

- **SetServiceObjectSecurity / per-service DACL**: a service object carries its own security descriptor, and the SCM grants access per the *service* DACL. By adding an ACE granting a user `SERVICE_START`/`SERVICE_STOP`, that **non-admin user can start/stop the service** (the standard `sc sdset` / `SetServiceObjectSecurity` pattern). The dangerous bits (`SERVICE_CHANGE_CONFIG`, `DELETE`, `WRITE_DAC`, `WRITE_OWNER`) must stay admin-only — `SERVICE_CHANGE_CONFIG` lets the holder rewrite `lpBinaryPathName`, i.e. run arbitrary code as the service account (LocalSystem) = local privilege escalation.
  https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-setserviceobjectsecurity / https://learn.microsoft.com/en-us/windows/win32/services/service-security-and-access-rights
- **SERVICE_DEMAND_START / ChangeServiceConfig**: `CreateService` with `dwStartType = SERVICE_DEMAND_START` registers a manual-start service (no boot launch); `ChangeServiceConfigW(SERVICE_NO_CHANGE, SERVICE_DEMAND_START, …)` migrates an existing AUTO_START registration. A stopped demand-start service is an inert SCM database row — no process, no RAM.
  https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-createservicew
- **MoveFileEx + MOVEFILE_DELAY_UNTIL_REBOOT**: schedules a delete (lpNewFileName = NULL) processed at next boot via `PendingFileRenameOperations`; "can be used only … by a member of the Administrators group or LocalSystem". The standard idiom for a running image deleting itself (the SYSTEM GC removing its own `%ProgramData%` binary + dir).
  https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw
- **Task Scheduler as SYSTEM**: a task with principal `S-1-5-18` (LocalSystem) + `RunLevel=HighestAvailable` runs unattended with full privilege; `StartWhenAvailable=true` runs a missed daily trigger when the machine is next on. Registering from XML (`schtasks /Create /XML`) keeps `<Command>`/`<Arguments>` as separate elements, avoiding `/TR` command-line quoting pitfalls. Registration itself requires elevation (done during the one-time elevated install).
  https://learn.microsoft.com/en-us/windows/win32/taskschd/daily-trigger-example--xml-

## Regex engine (rust `regex` crate, researched 2026-06-15, premise for first-class status = ADR-0023)

- **Linear-time guarantee, no ReDoS**: the `regex` crate is implemented with finite automata (lazy DFA / Pike VM) and **does not backtrack**. Matching is **linear** in "input length × pattern length", and the catastrophic backtracking that plagues regex services (ReDoS runtime exponential blowup) **cannot occur structurally**, as officially stated. Even malicious `(a+)+$`-style patterns run linearly.
  https://docs.rs/regex/latest/regex/#untrusted-input
  https://docs.rs/regex/latest/regex/#performance
- **Remaining attack surface = compile time/memory**: when accepting untrusted patterns, the only DoS surface is the **compile-time program/DFA size** demanded by a huge pattern (bounded-repetition expansion like `a{1000}{1000}`). The crate provides `RegexBuilder::size_limit` (byte cap on the compiled program, **default 10 MiB**) and `dfa_size_limit` (byte cap on the lazy DFA cache, **default 2 MiB**); on overflow `build()` returns an `Error` (`CompiledTooBig` equivalent). `nest_limit` (default 250) caps parse-tree depth. The docs recommend **tightening both size limits** for untrusted patterns.
  https://docs.rs/regex/latest/regex/struct.RegexBuilder.html#method.size_limit
  https://docs.rs/regex/latest/regex/struct.RegexBuilder.html#method.dfa_size_limit
- find-my-files uses 1 MiB each (with name length p99 ≈110B this is excessively generous; legitimate patterns never reach it, and malicious patterns are cleanly rejected with `FMF_E_QUERY_SYNTAX`). Decision and re-examination triggers are in ADR-0023.

## MSIX packaging (researched 2026-06-24, premise for ADR-0028, primary sources confirmed)

The hybrid "packaged UI + unpackaged service" decision (ADR-0028) rests on these. The two decisive facts are the service extension exposing none of the ADR-0017/0027 controls, and single-project MSIX holding only one executable.

- **`PublishSingleFile` is unsupported for packaged and framework-dependent WinUI 3**; it works only for unpackaged + self-contained apps, and even then the native WinAppSDK helpers stay loose (fewer files, not one exe). MSIX therefore cannot use it, so the portable-zip story and a future MSIX story are independent.
  https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/deploy-overview
- **The MSIX service extension `desktop6:Service`** can register a service, but its only attributes are `Name` / `StartupType` (auto|manual|disabled) / `StartAccount` (localSystem|localService|networkService) / `Arguments` + child Dependencies/TriggerEvents. There is **no** attribute for a custom service-object DACL, `SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO` privilege stripping, `SERVICE_PRESHUTDOWN_INFO`, install-time SID capture, directory DACL hardening, or scheduled-task creation; install/uninstall is owned by the MSIX deployment engine (requires the `localSystemServices` restricted capability). → a *packaged* service forfeits the entire ADR-0017/0027 hardening; the service must stay unpackaged.
  https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop6-service
- **A child process a packaged app launches from outside the package does not inherit package identity** — it runs as a normal full-trust desktop process (free to write `%ProgramData%`, register the SCM service, set DACLs, strip privileges, create scheduled tasks). Only an exe that lives *inside* the package runs with package identity, and MSIX filesystem redirection (AppData copy-on-write, VFS) applies to package-identity processes only. → the packaged UI's elevated `fmf-service install` helper keeps doing exactly what ADR-0027 does today.
  https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes
- **MSIX install trust comes from a publicly-trusted certificate chain, not the validation tier.** An MSIX signed by a cert chaining to a public-CA root installs by App Installer double-click without the user importing anything, **provided the manifest `Publisher` exactly equals the certificate Subject DN**. IV vs OV vs EV is irrelevant to install-trust (it affects only SmartScreen reputation). → the active SSL.com IV cert (`CN=Yasunobu Sakashita`, ADR-0020) suffices; the manifest Publisher must match it verbatim.
  https://learn.microsoft.com/en-us/windows/msix/package/sign-msix-package-guide
  https://learn.microsoft.com/en-us/windows/msix/package/create-certificate-package-signing
- **winget**: the community repo (`microsoft/winget-pkgs`) accepts MSIX/MSIXBundle/MSI/exe; portable/zip is not the safe submission path, and when several installer types are offered the client prefers MSIX > MSI > exe > portable. Self-hosting needs only a YAML manifest pointing at the GitHub Release asset URL + SHA256 (no Store account). → MSIX is the channel that unlocks winget; the zip stays the portable channel.
  https://github.com/microsoft/winget-pkgs
- **Single-project MSIX bundles exactly one executable**; combining multiple exes (UI + CLI + service) requires a classic Windows Application Packaging Project (`.wapproj`). A native payload DLL (`fmf_engine.dll`) alongside the single app exe is fine either way. → the package carries the UI apphost + `fmf_engine.dll` only; `fmf.exe`/`fmf-service.exe` ship out-of-band.
  https://learn.microsoft.com/en-us/windows/apps/windows-app-sdk/single-project-msix
- **Filesystem redirection**: for a package-identity process, `%APPDATA%`/`%LOCALAPPDATA%` writes are copy-on-write redirected to `…\AppData\Local\Packages\<PackageFamilyName>\…` (AppData has no VFS — pure COW); `%ProgramData%` is neither auto-redirected nor auto-merged without the Package Support Framework. → the packaged UI must force its profile path (portable `<exe>\data` is dead under read-only `WindowsApps`), while the de-identified LocalSystem service's `%ProgramData%` tree is unaffected.
  https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-behind-the-scenes
- **Self-contained vs framework-dependent MSIX**: framework-dependent is smaller but needs the Windows App Runtime present on the target (no Store resolver on the sideload/winget self-host path); self-contained carries WinAppSDK in-package and installs with nothing pre-present, but is not serviceable for WinAppSDK CVEs (rebuild to patch). → self-contained chosen (matches the existing `WindowsAppSDKSelfContained=true`); framework-dependent is the package-size re-examination trigger.
  https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/deploy-overview

## Code signing — CI integration (researched 2026-06-25, premise for ADR-0029)

ADR-0020 fixed the provider/cert (SSL.com eSigner, individual IV). These facts drove the *mechanism* (official Action) and the `build`→`sign`→`publish` split:

- **Azure Trusted Signing (the 2026 managed standard) is unavailable here.** Individual-developer onboarding is **paused**, and new tenants are limited to **US/CA organizations with 3+ years of verifiable history** — a Japanese individual cannot enroll. This closes the door ADR-0020 already noted (it was US/CA/EU/UK individual-only before).
  https://techcommunity.microsoft.com/blog/microsoft-security-blog/trusted-signing-is-now-open-for-individual-developers-to-sign-up-in-public-previ/4273554 / https://learn.microsoft.com/en-us/answers/questions/5810735/cant-create-a-new-trusted-signing-individual-ident
- **`dotnet sign` only delegates to Azure Key Vault / Trusted Signing.** The modern Microsoft CLI computes a digest and submits it to AKV/Trusted Signing for RSA signing; it has **no SSL.com eSigner backend**. So the "most standard" CLI is not usable with this cert.
  https://github.com/dotnet/sign
- **The official `SSLcom/esigner-codesign` Action (`batch_sign`) is SSL.com's recommended CI integration, and it works with this account.** It downloads CodeSignTool, runs `scan_code` (pre-signing malware scan, required when the account has the malware blocker on), signs, and timestamps via SSL.com's TSA. Only file hashes leave the runner. Proven green on this cert in CI (run `28082306344`). It is SHA-pinnable (v1.3.2). `sign`/`batch_sign` also accept `.msix`.
  https://github.com/SSLcom/esigner-codesign
- **eSigner CKA (Cloud Key Adapter) was tried and fails in CI.** CKA loads the cloud cert into the Windows store via a CNG KSP so the standard `signtool` can sign — but the KSP is **32-bit** (x64 signtool reports "No certificates were found…"; x86 is required, per SSL.com), and even with x86 the sign call fails at credential retrieval: `Signing credentials not configured … SignerSign() failed (0x80090003)`. This is a CKA-internal CSC credential path, **not** an account/PIN problem (the Action's `batch_sign` signs fine on the same account). Rejected for CI; see ADR-0029.
  https://github.com/SSLcom/eSignerCKA / https://www.ssl.com/how-to/how-to-integrate-esigner-cka-with-ci-cd-tools-for-automated-code-signing/
- **`signtool verify /pa /tw` is the authoritative timestamp check.** Exit **0** = chain valid + timestamped, **2** = signed but **not** timestamped, **1** = invalid — so any non-zero fails the build, catching a missing RFC 3161 timestamp (without which a signature dies with the ~460-day cert).
  https://learn.microsoft.com/en-us/windows-hardware/drivers/devtest/signtool
- **`Get-AuthenticodeSignature.TimeStamperCertificate` is unreliable for this assertion**: it is null under `-FilePath` on the runner (a long-standing PowerShell bug), so the timestamp guarantee must come from `signtool`, not this property. The cmdlet is still fine for the signer-subject check.
  https://github.com/PowerShell/PowerShell/issues/4060
