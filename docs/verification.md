# Verification scope

The [proof inventory](../verification/proofs.json) defines the executable scope.
`just --list` describes the available entry points.
CI requires the applicable lanes through its existing required status checks.

Frame-header proofs check field and byte round-trips and an independent little-endian layout for every field value and header byte.
Volume-label proofs check prefix preservation, zero padding, termination, and first-zero slicing for every field byte and bounded array input.

Update-sequence proofs check that array and sector accesses stay inside the record.
The domain includes every native unsigned geometry value, every encoded header field, and arbitrary valid word and sector indices.
Boot-sector proofs check derived sizes for overflow and verify that a complete record fits inside the volume for every byte and shorter prefix.
Attribute-header proofs check record and name ranges for every byte and prefix of the bounded harness buffer, excluding larger buffers.

Proof-completion checks accept only a complete, successful, nonempty harness count across all native unsigned counts and expected inventory sizes.
Snapshot-column proofs check initialized readable bytes and prefix preservation during growth for every stored integer column type.
The domain stays within reserved capacity and the harness element bound, with arbitrary prefix bytes for the widest column.

`WTF-8` slice proofs check prefix and unused-suffix preservation, bounded writes, and unchanged output on rejection.
They cover every byte sequence of each length from zero through four with arbitrary existing UTF-16 output units.
Append-length proofs check prefix preservation on rejection and overflow rejection for every native unsigned initial length and decoded unit count.

Histogram-bucket proofs check valid indices and sample containment for every native unsigned slot and unsigned 64-bit sample.
Recording proofs check saturating totals and changes confined to the selected bucket for arbitrary unsigned 64-bit state and samples.
The state includes bucket counters, total count, sum, and the largest recorded sample.

The harness source defines each array and unwind bound.
Snapshot column proofs check each fixed length within their bound to avoid the verifier's [symbolic heap read-back cost](https://github.com/model-checking/kani/issues/4667).
`WTF-8` decoder proofs check the same complete bounded byte domain at separate fixed lengths.
The production decoder validates input before writing through its slice kernel.
The Vec adapter uses that kernel and the proved append-length decision.
Direct heap-based decoder harnesses exceeded the declared budget even for fixed lengths, so these harnesses use caller-owned arrays without stubbing the decoder.
They compare output units individually to avoid the prover library's byte-wise `memcmp` loop.
Vec allocation, resize, and truncate follow their standard library contracts as trust assumptions.
Native tests, Miri, and the UTF-16 round-trip fuzz target exercise the actual Vec adapter.
Reassess that adapter boundary when the pinned verifier's heap model or the adapter changes.
The proof runner checks that Rust declarations match the inventory and requires successful assertions and satisfiable cover cases.
Unsupported operations reached by a harness fail verification.
The result receipt includes source hashes and hashes of the original prover reports.
Changing an input file invalidates the receipt.

The standard library's UTF-8 validation exceeded the configured Kani budget during initial verification.
The production label functions separate byte conversion from UTF-8 interpretation.
Kani checks the byte conversion functions.
Miri and native tests exercise valid Unicode, invalid bytes, split scalars, and drive labels through the public API.
This evidence doesn't constitute an exhaustive proof of the standard library's UTF-8 implementation.
The contract of `core::str::from_utf8` remains an explicit trust assumption at this boundary.
Reassess this boundary when the decoder changes or the pinned verifier can cover that library call within the declared budget.

Miri runs the contract library's tests with required boundary cases present and with default isolation and borrow checks.
It also executes a snapshot reader that inspects each destination buffer before filling it.
The [`Read` contract](https://doc.rust-lang.org/std/io/trait.Read.html#method.read) requires callers to initialize these buffers.
Snapshot columns use `bytemuck::Pod` to exclude padding and invalid bit patterns, and the snapshot module forbids unsafe code.
The reader initializes one bounded chunk at a time before calling external code.
The separate configurations under `verification/` pin the compatible Linux toolchains without changing the normal development toolchain.
Pull requests and merge groups run the fast source gates and check the proof inventory.
The Release workflow reuses CI to execute all registered proofs, interpreter checks, and fuzz targets on the admitted candidate commit.
Full verification overrides path filters and must succeed before the signing stage prepares its inputs.
The summary gate rejects failed, cancelled, skipped, or missing required candidate checks.
Fuzzing remains available through manual dispatch without a periodic schedule.
[Release candidate assurance](adr/release-candidate-assurance.md) records the verification stages.

Fuzzing replays the committed corpora before its existing bounded exploration.
The runner generates a workspace manifest under the canonical build tree and keeps its dependency and target paths bound to the original source files.
This accommodates cargo-fuzz's writable artifact directory while keeping the checkout read-only during Linux verification.
The committed and generated lockfiles must remain identical.
Manifest parsing, Cargo resolution, filesystem behavior, and process execution follow their external library and operating-system contracts.
Native manifest tests check source identity and retained settings, and the read-only Linux replay exercises the real adapter.
Reassess this boundary when the manifest structure or cargo-fuzz's project handling changes.
The wire and journal targets check encoding round-trips.
The snapshot target checks stable re-encoding.
The query target checks literal reconstruction and whitespace invariance.
The file-record target checks the decoded name bytes.
Boot-sector and extent targets check accepted geometry and volume bounds.
The list target compares flat decoding with streaming across reversed physical runs and checks cancellation.
The `WTF-8` target checks UTF-16 round-trips and atomic rejection of arbitrary bytes.
The strict JSON target includes the production decoder used by mutation evidence and checks that successful decoding matches `serde_json`.
All JSON workspaces use the library's `float_roundtrip` feature to preserve finite floating-point values across serialization and decoding.
Numeric regression tests compare the decoded bits with Rust's floating-point parser and preserve the original fuzz input.
New regressions belong in the matching corpus.
Regeneration or mutation campaigns must preserve existing minimized inputs.

Windows handle ownership, access-control behavior, service installation, UI behavior, and concurrent schedules require their native tests and applicable integration checks.
The pure proofs assume the documented Windows API contracts at those adapters.
The UI settings adapter waits for the dialog to disappear and the options button to become enabled before another open request.
Each failed invocation or readiness check stops the current operation and preserves its exit code.
The C# lifecycle tests load the actual PowerShell function definitions through the [PowerShell parser](https://learn.microsoft.com/en-us/dotnet/api/system.management.automation.language.parser.parsefile) and control command outcomes and dismissal progress.
The fixtures reject premature completion during delayed dismissal and every failed command phase without launching a window.
Those checks establish adapter sequencing under the WinApp CLI contract, while `just ui-test` exercises real WinUI dismissal and input behavior on Windows.
Reassess the adapter when the CLI readiness semantics or WinUI modal lifecycle changes.
Elevated security tests and the real-volume performance gate keep their existing merge requirements.
Full mutation campaigns use the reviewed baseline with bounded worker and time budgets.

Recovery harnesses invoke the production transitions for every retry state and failure class.
The delay harness quantifies every valid `Duration` and checks expiration and the remaining delay without overflow.
The runtime supplies elapsed clock time to that decision and uses a condition variable for request and shutdown notifications.
Native tests exercise the public retry request from a failure callback, bounded automatic recovery, panic recovery, and stopping retry waits.
The native schedule tests use the existing snapshot and journal seams.
A synchronization gate exists only in test builds and pauses a pending flush before it reads snapshot data.
The test replaces the index and checkpoint during that pause, then checks the pair delivered to the store.
Production persistence holds a single data read guard through the save, so replacement cannot separate the checkpoint from its index.
