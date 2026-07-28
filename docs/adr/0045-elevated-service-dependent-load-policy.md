# ADR-0045: System32-only static imports for the elevated service

Date: 2026-07-26 / Status: Accepted

## Context

The UI launches the bundled `fmf-service.exe` through UAC for initial
installation. At that instant the signed EXE and its parent directories are
locked and identity-checked, but the extracted bundle is still user-writable.
An EXE-only check does not protect a statically imported DLL resolved from the
application directory. Microsoft identifies this as DLL planting and documents
`/DEPENDENTLOADFLAG:0x800` (`LOAD_LIBRARY_SEARCH_SYSTEM32`) as the linker-level
mitigation:

- [MSVC `/DEPENDENTLOADFLAG`](https://learn.microsoft.com/en-us/cpp/build/reference/dependentloadflag)
- [Dynamic-link library security](https://learn.microsoft.com/en-us/windows/win32/dlls/dynamic-link-library-security)
- [PE format](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format)

The service previously imported `VCRUNTIME140.dll`, so merely selecting
System32 would also create an undeclared VC Redistributable prerequisite.

## Decision

- All Windows MSVC Rust artifacts link the CRT statically.
- The `fmf-service` binary alone receives
  `/DEPENDENTLOADFLAG:0x800` from its crate build script. It is the only binary
  elevated while still in the extracted bundle.
- `xtask` parses `IMAGE_LOAD_CONFIG_DIRECTORY.DependentLoadFlags` without
  trusting `dumpbin` availability. Publish checks the source service before the
  app embeds its image digest and checks the copied service again. Package
  repeats the check independently before writing the ZIP. The required value is
  exactly `0x800`.
- Release signing may alter only Authenticode-excluded bytes; collection checks
  every first-party PE before any bundle overwrite and again afterward.

## Consequences

The elevated helper has no adjacent private runtime DLL to load, and its
remaining static imports resolve only from System32. A missing linker flag,
truncated/malformed PE, stale pre-change bundle, substituted signer result, or
future extra load flag fails the release pipeline closed.

This governs static imports. Any future explicit DLL load must use an absolute
trusted path or safe `LoadLibraryEx` flags and requires a new threat-model
review; plugins remain out of scope.

## Rejected alternatives

- **Rely on Authenticode and EXE locks.** They authenticate the image, not DLLs
  selected later by the loader.
- **Ship a private VC runtime beside the helper.** That restores the writable
  adjacent-DLL attack surface.
- **Apply the linker flag to every PE.** Only the pre-install service crosses
  this UAC boundary; a crate-scoped flag keeps unrelated load behavior explicit.
- **Trust a workflow-only `dumpbin` check.** Local publish/package must enforce
  the same invariant, and release packaging must not be able to bypass publish.
