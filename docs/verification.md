# Verification scope

The [proof inventory](../verification/proofs.json) defines the executable scope.
The task listing from `just --list` describes the available entry points.
CI requires the applicable lanes through its existing required status checks.

Frame-header proofs check field and byte round trips and an independent little-endian layout for every field value and header byte.
Volume-label proofs check prefix preservation, zero padding, termination, and slicing at the first zero.
Their domain includes every field byte and inputs within the harness array bound.

Update-sequence proofs keep array and sector accesses inside the record for every native unsigned geometry value and encoded header field.
They include arbitrary valid word and sector indices.
Boot-sector proofs check size arithmetic and room for a complete record for every boot-sector byte and shorter prefix.
Attribute-header proofs keep record and name ranges inside each bounded buffer for every byte and shorter prefix.
They make no claim about larger buffers.

Proof-completion checks require a complete, successful, nonempty harness count for every native unsigned count and expected inventory size.
Snapshot-column proofs check initialized readable bytes and prefix preservation during growth.
They cover every stored integer column type within reserved capacity and the harness element bound, with arbitrary prefix bytes for the widest column.

Slice-decoding proofs for `WTF-8` preserve the prefix and unused suffix, bound writes, and leave output unchanged on rejection.
They cover every byte sequence of each length from zero through four, with arbitrary existing UTF-16 output units.
Append-length proofs preserve the prefix on rejection and reject overflow for every native unsigned initial length and decoded unit count.

Histogram proofs keep bucket indices in range and contain each sample in its selected interval for every native unsigned slot and unsigned 64-bit sample.
Recording proofs saturate totals without overflow and change only the selected bucket.
Their domain includes arbitrary unsigned 64-bit samples, bucket counters, total counts, sums, and maxima.

The harness source defines each array and unwind bound.
Snapshot column proofs check each fixed length within their bound to avoid the verifier's [symbolic heap read-back cost](https://github.com/model-checking/kani/issues/4667).
Decoder proofs for `WTF-8` check the same complete bounded byte domain at separate fixed lengths.
The production decoder validates input before writing through its slice kernel.
The vector adapter uses that kernel and the checked append-length decision.
Direct heap-based decoder harnesses exceeded the declared budget even for fixed lengths, so these harnesses use caller-owned arrays without stubbing the decoder.
They compare individual output units to avoid comparing buffers byte by byte.
The vector operations for allocation, resizing, and truncation depend on their standard library contracts.
Native tests, Miri, and the UTF-16 round-trip fuzz target exercise the actual vector adapter.
Reassess that adapter boundary when the pinned verifier's heap model or the adapter changes.
The proof runner checks that Rust declarations match the inventory and requires successful assertions and satisfiable cover cases.
A harness that reaches an unsupported operation fails verification.
The result receipt includes source hashes and hashes of the original verifier reports.
Changing an input file invalidates the receipt.

The standard library's UTF-8 validation exceeded the configured Kani budget during initial verification.
The production label functions separate byte conversion from UTF-8 interpretation.
Kani checks the byte conversion functions.
The interpreter and native tests exercise valid Unicode, invalid bytes, split scalars, and drive labels through the public API.
This evidence doesn't constitute an exhaustive proof of the standard library's UTF-8 implementation.
The string conversion depends on the standard library contract for `core::str::from_utf8`.
Reassess this boundary when the decoder changes or the pinned verifier can cover that library call within the declared budget.

The interpreter lane runs the contract library's tests with required boundary cases present and with default isolation and borrow checks.
It also executes a snapshot reader that inspects each destination buffer before filling it.
The [reader contract](https://doc.rust-lang.org/std/io/trait.Read.html#method.read) requires initialized buffers.
The column representation uses `bytemuck::Pod` to exclude padding and invalid bit patterns.
The snapshot module forbids unsafe code.
The reader initializes one bounded chunk at a time before calling external code.
The Linux verification tools use separate configurations without changing the normal development toolchain.
Their configurations live under `verification/`.
The Windows source gate checks the proof inventory.
The Linux CI lane executes the proofs and interpreter tests.

Fuzzing replays the committed corpora before its existing bounded exploration.
The runner generates a workspace manifest under the canonical build tree and keeps its dependency and target paths bound to the original source files.
The generated manifest gives the fuzz tool a writable artifact directory while keeping the checkout read-only during Linux verification.
The committed and generated lock files must remain identical.
Manifest parsing, Cargo resolution, file system behavior, and process execution depend on external library and operating-system contracts.
Native manifest tests check source identity and retained settings, and the read-only Linux replay exercises the real adapter.
Reassess this boundary when the manifest structure or cargo-fuzz's project handling changes.
The wire and journal targets check encoding round-trips.
The snapshot target checks stable re-encoding.
The query target checks literal reconstruction and whitespace invariance.
The file-record target checks the decoded name bytes.
Boot-sector and extent targets check accepted geometry and volume bounds.
The list target compares flat decoding with streaming across reversed physical runs and checks cancellation.
The encoding target for `WTF-8` checks UTF-16 round trips and atomic rejection of arbitrary bytes.
The strict JSON target includes the production decoder used by mutation evidence.
It compares successful decoding with the result from `serde_json`.
The JSON workspaces enable `float_roundtrip` to preserve finite floating-point values across serialization and decoding.
Numeric regression tests compare the decoded bits with Rust's floating-point parser and preserve the original fuzz input.
New regressions belong in the matching corpus.
Regeneration or mutation campaigns must preserve existing minimized inputs.

Native tests and applicable integration checks cover Windows handle ownership, access-control behavior, service installation, UI behavior, and concurrent schedules.
The pure proofs assume the documented Windows API contracts at those adapters.
Elevated security tests and the real-volume performance gate keep their existing merge requirements.
Full mutation campaigns use the reviewed baseline with bounded worker and time budgets.

Run `just stryker-smoke` to print the one-file activation plan without restoring tools or changing output.
Run `just stryker-smoke run` to execute the plan with the shared Stryker configuration and app test profile.
The smoke requires a real `Killed` mutant and rejects survivors, timeouts, missing coverage, unapproved ignored results, and activation outside the selected file.
Compile rejections don't count as executed kills.
The runner preserves the report hash and source identity before and after the run.
Its separate evidence carries `full_campaign=false` and supplies no full-campaign acceptance.
The activation harness checks the production predicate for every native unsigned outcome count.
The mutation tool and report parser remain external and integration boundaries.

The C# suite uses the released `xunit.v3` package with its native Microsoft Testing Platform (MTP) integration.
The executable test project enables that runner for Stryker's mutation control.
The ordinary test command keeps the Visual Studio adapter, the coverage gate, and the existing pipe settings.
The project declares its MSBuild target as `VSTest` and enables the native MTP executable entry point.
Microsoft Testing Platform 2 rejects the VSTest target under .NET 10.
The suite turns off collection parallelism.
The named pipe connection contract collection runs before client tests that require a working connection.
The [ordering operation](https://learn.microsoft.com/en-us/dotnet/api/system.linq.enumerable.orderbydescending?view=net-10.0) preserves the relative order of collections with the same priority.
Every collection remains in the test run.
Rejection tests drain an unexpected frame so an incorrectly admitted native write can finish and fail its assertion.
Each test cancels and joins its peer task before stream disposal.
The copied xUnit runner configuration stops further tests after a reported failure.
Successful baselines still execute the complete suite.
The mutation controller seals the configuration and rejects a missing, changed, or untracked copy.
The sanitized work directory receives the controller's canonical file.
Validation rejects alternate xUnit and MTP configuration files that the runner discovers automatically.
The runner stops scheduling tests after a reported failure.
Native controls run the complete executable suite with admission changes and omitted write operations.
The xUnit collection extension and Windows pipe completion remain external contracts checked by the native controls.
Timeouts and incomplete reports remain inconclusive.
Stryker's MTP adapter is a preview integration.
Migration requires the real one-file activation check and ordinary, coverage, and pipe runs.
The external runner contracts come from the [xUnit migration guide](https://xunit.net/docs/getting-started/v3/migration), [platform integration guide](https://xunit.net/docs/getting-started/v3/microsoft-testing-platform), and [Stryker runner configuration](https://stryker-mutator.io/docs/stryker-net/configuration/#test-runner-string).
The source-bound activation predicate proof covers the acceptance decision.
It doesn't prove the framework or its adapter.

Both Rust mutation lanes derive mutant jobs and test threads for `nextest` from one nonzero CPU estimate.
The production planner reserves at most four test threads per job.
The combined thread count stays within the estimate, including hosts with one, two, or three CPUs.
Kani checks positive counts, the thread cap, unused-slot bounds, and multiplication without overflow for every nonzero native unsigned CPU count.
The operating system supplies the CPU estimate.
Compiler workers and thread pools inside individual tests keep their own resource policies.
Argument tests check the actual local and CI vectors, including placement of each concurrency flag and the shared timeout floor.
Timeouts and incomplete mutation reports still fail the existing exact-identity gate.

Recovery harnesses invoke the production transitions for every retry state and failure class.
The delay harness quantifies every valid duration and checks when the wait ends and how much time remains without overflow.
The runtime supplies elapsed clock time to that decision and uses a condition variable for request and shutdown notifications.
Native tests exercise the public retry request from a failure callback, bounded automatic recovery, panic recovery, and stopping retry waits.
The native schedule tests use the existing snapshot and journal seams.
Test builds can pause a pending flush before it reads snapshot data.
The test replaces the index and checkpoint during that pause, then checks the pair delivered to the store.
Persistence holds one read guard while saving the index and checkpoint.
Replacement can't split the pair during the save.

The managed-content parser uses a production range validator for Common Language Runtime (CLR) table extents.
Its Kani harness checks accepted ranges and complete rejection conditions for every native unsigned start, row count, row width, and buffer length.
A second harness checks every byte sequence up to four bytes for accepted compressed-integer value, canonical width, and exact prefix consumption.
The scalar proof doesn't establish semantic validity under `ECMA-335` or correctness of generated native code.
Native regressions cover relocated IL and initialized fields, exception sections, managed resources, malformed layouts, and changed content.
Candidate SBOM reconciliation compares the actual published assemblies with the locked NuGet package archives.
The [managed identity decision](adr/relocation-independent-managed-identity.md) records the supported boundary and compiler trust assumption.

The product-version proof invokes the production byte scanner for every byte sequence up to its array bound.
It checks that the returned end stays inside the input, every earlier byte is outside the metadata delimiter set, and any ending delimiter is canonical.
The string adapter slices at that end.
Either ASCII delimiter starts at a UTF-8 character boundary.
Native tests cover multibyte text and both delimiter orders.
The earlier direct string-pattern harness exceeded its declared solver budget, so the scanner exposes the same decision without the standard library's pattern-search machinery.
The proof budget and accepted input bound remain unchanged.

The release overflow experiment follows [ADR-0013](adr/0013-measurement-discipline.md).
Inspect its variants, fixed order, output location, and cooling intervals with `just overflow-experiment`.
The [Cargo profile reference](https://doc.rust-lang.org/cargo/reference/profiles.html#overflow-checks) defines the compiler option.
The experiment preserves the existing boundary overrides and Windows Control Flow Guard settings.
Variants A and B explicitly turn off the release default before applying their selected package overrides.
Variant C explicitly enables the release default.
The comparison scopes stay distinct after the shipping profile expands.

Build and hash the comparison binaries from a clean committed source with `just overflow-experiment prepare`.
Preparation restores the ordinary release tool after preserving each candidate.
The manifest binds the compiler, dependency graph, source, profile arguments, machine identity, and executable hashes.
Preparation rejects ambient compiler and profile overrides.
Preparation changes no committed profile and performs no restart.

Measure with `just overflow-experiment measure` only in the owner's agreed quiet window after an owner-controlled restart that follows preparation.
The runner checks the recorded source and executables, the boot timestamp, and the process affinity before every measured command.
The recorded logical processor count must match the process affinity on the measurement host.
The experiment depends on Windows clock and affinity APIs, the file system, the compiler, Cargo, and cryptographic libraries.
Their behavior remains an external trust assumption.
Native tests and the real preparation and measurement commands exercise those boundaries.
An unavailable or inconsistent observation stops the experiment without acceptance.

Each measured command uses the existing cold preflight and continuous counters.
An idle check follows measurement.
Every round compares saved reports with its own A control, so reversed rounds need no extra volume scan.
The runner compares control reports with the first round's control report.
The existing real-volume evaluator runs through `fmf bench-compare` and writes a failed verdict before returning failure.
Passing controls, malformed reports, and a known over-budget report exercise that command.
The experiment keeps valid negative performance results and collects every scheduled round before reporting a failed performance gate.
Partial, stale, or thermally invalid evidence remains inconclusive.
No experiment result authorizes a profile expansion without reviewing the paired costs and existing acceptance conditions.

The production-bound Kani harnesses check every scheduled round, every variant's exact compiler arguments, and the boot predicate for all admitted timestamps.
The operating system supplies the timestamp values.
The proofs establish the deterministic guards and don't establish an empirical performance claim.
The full mutation campaign remains at the Release gate.
