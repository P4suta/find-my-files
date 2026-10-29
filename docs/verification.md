# Verification scope

The [proof inventory](../verification/proofs.json) defines the executable scope.
`just --list` describes the available entry points.
CI requires the applicable lanes through its existing required status checks.

| Boundary | Checked property | Input domain and limit |
| --- | --- | --- |
| Frame header | Field and byte round-trips, independent little-endian layout | Every field value and every header byte |
| Volume label bytes | Prefix preservation, zero padding, termination, first-zero slicing | Every field byte and inputs within the harness array bound |
| Update-sequence layout | Array and sector accesses remain inside the record | Every native unsigned geometry value, every encoded header field, and arbitrary valid word and sector indices |
| Boot-sector geometry | Derived sizes don't overflow and a complete record fits inside the volume | Every boot-sector byte and every shorter prefix |
| Attribute header | Declared record and name ranges remain inside the buffer | Every byte and prefix of the bounded harness buffer, excluding larger buffers |
| Proof completion | Accept only a complete, successful, nonempty harness count | Every native unsigned count and expected inventory size |
| Snapshot column buffers | Initialize readable bytes and preserve the existing prefix during growth | All stored integer column types within reserved capacity and the harness element bound, with arbitrary prefix bytes for the widest column |
| WTF-8 slice decoding | Preserve the prefix and unused suffix, bound writes, and leave output unchanged on rejection | Every byte sequence of each length from zero through four, with arbitrary existing UTF-16 output units |
| Decode append length | Preserve the prefix on rejection and reject length overflow | Every native unsigned initial length and decoded unit count |
| Histogram buckets | Keep bucket indices in range and contain each sample in its selected interval | Every native unsigned slot and every unsigned 64-bit sample |
| Histogram recording | Saturate totals without overflow and change only the selected bucket | Arbitrary unsigned 64-bit sample, bucket counters, total count, sum, and maximum |

The harness source defines each array and unwind bound.
Snapshot column proofs check each fixed length within their bound to avoid the verifier's [symbolic heap read-back cost](https://github.com/model-checking/kani/issues/4667).
WTF-8 decoder proofs check the same complete bounded byte domain at separate fixed lengths.
The production decoder validates input before writing through its slice kernel.
The Vec adapter uses that kernel and the proved append-length decision.
Direct heap-based decoder harnesses exceeded the declared budget even for fixed lengths, so these harnesses use caller-owned arrays without stubbing the decoder.
They compare output units individually to avoid the prover library's byte-wise `memcmp` loop.
Vec allocation, resize, and truncate retain their standard library contracts as trust assumptions.
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
The Windows source gate checks the proof inventory.
The Linux CI lane executes the proofs and interpreter tests.

Fuzzing replays the committed corpora before its existing bounded exploration.
The runner generates a workspace manifest under the canonical build tree and keeps its dependency and target paths bound to the original source files.
This accommodates cargo-fuzz's writable artifact directory while keeping the checkout read-only during Linux verification.
The committed and generated lockfiles must remain identical.
Manifest parsing, Cargo resolution, filesystem behavior, and process execution retain their external library and operating-system contracts.
Native manifest tests check source identity and retained settings, and the read-only Linux replay exercises the real adapter.
Reassess this boundary when the manifest structure or cargo-fuzz's project handling changes.
The wire and journal targets check encoding round-trips.
The snapshot target checks stable re-encoding.
The query target checks literal reconstruction and whitespace invariance.
The file-record target checks the decoded name bytes.
Boot-sector and extent targets check accepted geometry and volume bounds.
The list target compares flat decoding with streaming across reversed physical runs and checks cancellation.
The WTF-8 target checks UTF-16 round-trips and atomic rejection of arbitrary bytes.
The strict JSON target includes the production decoder used by mutation evidence and checks successful decoding against `serde_json`.
All JSON workspaces use the library's `float_roundtrip` feature to preserve finite floating-point values across serialization and decoding.
Numeric regression tests compare the decoded bits with Rust's floating-point parser and preserve the original fuzz input.
New regressions belong in the matching corpus.
Regeneration or mutation campaigns must preserve existing minimized inputs.

Windows handle ownership, access-control behavior, service installation, UI behavior, and concurrent schedules require their native tests and applicable integration checks.
The pure proofs assume the documented Windows API contracts at those adapters.
Elevated security tests and the real-volume performance gate keep their existing merge requirements.
Full mutation campaigns use the reviewed baseline with bounded worker and time budgets.

`just stryker-smoke` prints the one-file activation plan without restoring tools or changing output.
`just stryker-smoke run` executes that plan with the shared Stryker configuration and app test profile.
The smoke requires a real `Killed` mutant and rejects survivors, timeouts, missing coverage, unapproved ignored results, and activation outside the selected file.
Compile rejections don't count as executed kills.
The runner preserves the report hash and source identity before and after the run.
Its separate evidence carries `full_campaign=false` and supplies no full-campaign acceptance.
Kani checks the production activation predicate for every native unsigned outcome count.
The mutation tool and report parser remain external and integration boundaries.

The C# suite uses xUnit.net v3 with its native Microsoft Testing Platform (MTP) v2 integration.
The executable test project enables that runner for Stryker's mutation control.
Ordinary `dotnet test` retains the Visual Studio adapter, the coverage gate, and the existing pipe runsettings.
The test project declares its MSBuild target as VSTest and separately enables the native MTP executable entry point.
This avoids running MTP on the legacy VSTest target, which MTP v2 rejects under .NET 10.
The suite keeps collection parallelism disabled.
Stryker's MTP adapter is a preview integration, so migration requires the real one-file activation check in addition to ordinary, coverage, and pipe runs.
The [xUnit migration guide](https://xunit.net/docs/getting-started/v3/migration), [platform integration guide](https://xunit.net/docs/getting-started/v3/microsoft-testing-platform), and [Stryker runner configuration](https://stryker-mutator.io/docs/stryker-net/configuration/#test-runner-string) define the external runner contracts.
The source-bound activation predicate proof covers the acceptance decision; it doesn't prove the framework or its adapter.

Both Rust mutation lanes derive mutant jobs and nextest test threads from one nonzero CPU estimate.
The production planner reserves at most four nextest threads per job and keeps their aggregate count within that estimate, including one-, two-, and three-CPU hosts.
Kani checks positive counts, the thread cap, unused-slot bounds, and multiplication without overflow for every nonzero native unsigned CPU count.
The operating system supplies the CPU estimate.
Compiler workers and thread pools started inside individual tests retain their own resource policies.
Argument tests check the actual local and CI vectors, including placement of each concurrency flag and the shared timeout floor.
Timeouts and incomplete mutation reports still fail the existing exact-identity gate.

Recovery harnesses invoke the production transitions for every retry state and failure class.
The delay harness quantifies every valid `Duration` and checks expiration and the remaining delay without overflow.
The runtime supplies elapsed clock time to that decision and uses a condition variable for request and shutdown notifications.
Native tests exercise the public retry request from a failure callback, bounded automatic recovery, panic recovery, and stopping retry waits.
The native schedule tests use the existing snapshot and journal seams.
A synchronization gate exists only in test builds and pauses a pending flush before it reads snapshot data.
The test replaces the index and checkpoint during that pause, then checks the pair delivered to the store.
Production persistence holds a single data read guard through the save, so replacement cannot separate the checkpoint from its index.

The managed-content parser uses a production range validator for CLR table extents.
Its Kani harness checks accepted ranges and complete rejection conditions for every native unsigned start, row count, row width, and buffer length.
A second harness checks every byte sequence up to four bytes for accepted compressed-integer value, canonical width, and exact prefix consumption.
That scalar proof doesn't establish ECMA-335 semantic validity or correctness of generated native code.
Native regressions cover relocated IL and initialized fields, exception sections, managed resources, malformed layouts, and changed content.
Candidate SBOM reconciliation exercises the actual published assemblies against the locked NuGet package archives.
The [managed identity decision](adr/relocation-independent-managed-identity.md) records the supported boundary and compiler trust assumption.

The product-version proof invokes the production byte scanner for every byte sequence up to its array bound.
It checks that the returned end is contained, every earlier byte is outside the metadata delimiter set, and any ending delimiter is canonical.
The string adapter slices at that end.
UTF-8 guarantees that either ASCII delimiter starts at a character boundary; native tests cover multibyte text and both delimiter orders.
The earlier direct string-pattern harness exceeded its declared solver budget, so the scanner exposes the same decision without the standard library's pattern-search machinery.
The proof budget and accepted input bound remain unchanged.
