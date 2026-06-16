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

## Regex engine (rust `regex` crate, researched 2026-06-15, premise for first-class status = ADR-0023)

- **Linear-time guarantee, no ReDoS**: the `regex` crate is implemented with finite automata (lazy DFA / Pike VM) and **does not backtrack**. Matching is **linear** in "input length × pattern length", and the catastrophic backtracking that plagues regex services (ReDoS runtime exponential blowup) **cannot occur structurally**, as officially stated. Even malicious `(a+)+$`-style patterns run linearly.
  https://docs.rs/regex/latest/regex/#untrusted-input
  https://docs.rs/regex/latest/regex/#performance
- **Remaining attack surface = compile time/memory**: when accepting untrusted patterns, the only DoS surface is the **compile-time program/DFA size** demanded by a huge pattern (bounded-repetition expansion like `a{1000}{1000}`). The crate provides `RegexBuilder::size_limit` (byte cap on the compiled program, **default 10 MiB**) and `dfa_size_limit` (byte cap on the lazy DFA cache, **default 2 MiB**); on overflow `build()` returns an `Error` (`CompiledTooBig` equivalent). `nest_limit` (default 250) caps parse-tree depth. The docs recommend **tightening both size limits** for untrusted patterns.
  https://docs.rs/regex/latest/regex/struct.RegexBuilder.html#method.size_limit
  https://docs.rs/regex/latest/regex/struct.RegexBuilder.html#method.dfa_size_limit
- find-my-files uses 1 MiB each (with name length p99 ≈110B this is excessively generous; legitimate patterns never reach it, and malicious patterns are cleanly rejected with `FMF_E_QUERY_SYNTAX`). Decision and re-examination triggers are in ADR-0023.
