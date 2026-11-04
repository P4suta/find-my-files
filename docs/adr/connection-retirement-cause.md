---
date: 2026-11-05
---

# Connection retirement cause

## Status

Proposed.
This refines the generation-scoped retirement behavior in [ADR-0022](0022-boundary-seams-behavioral-tests.md).

## Context

A request deadline and supervisor cleanup can retire the same pipe connection concurrently.
Closing the native stream wakes its reader before the retiring request necessarily fails the other pending requests.
If each caller supplies its own reason, cleanup can replace a request deadline with a generic disconnection message.
Requests admitted around retirement can also observe that generic message through the write path.

## Decision

The [connection](../../app/FindMyFiles/Engine/Transport/PipeConnection.cs) owns one retirement reason for its lifetime.
The retirement method uses [atomic comparison and exchange](https://learn.microsoft.com/en-us/dotnet/api/system.threading.interlocked.compareexchange?view=net-10.0).
Its first call records a non-null reason.
Every later call returns the recorded reason.
The winner is the first successful atomic publication.
The winner can differ from the request or supervisor that first noticed a failure.

The [client](../../app/FindMyFiles/Engine/Transport/PipeEngineClient.cs) records that reason before clearing the public connection slot or disposing the native stream.
Pending-request failures and write failures use the connection's recorded reason.
Retirement remains scoped to the captured connection object and its epoch.
A stale deadline can't clear a replacement connection or fail the replacement's pending requests.
Caller cancellation and disposal without a retirement cause keep their existing behavior.

The request clock is an optional constructor dependency.
Production constructors use the system clock.
The tests use Microsoft's pinned virtual clock and observe timer registration before advancing time.
Native pipe barriers force competing supervisor cleanup and retirement during an admitted write.
Each test bounds unresponsive operations with a diagnostic timeout.
Assertions depend on forced ordering.
The production reconnection delay still uses the system clock.

The changed state operation is a direct call to the .NET atomic primitive, rather than a separate decision algorithm.
Its atomic publication and return-value contract are external assumptions.
The platform provides pipe I/O, task scheduling, cancellation, and timers.
The Rust verifier doesn't cover those platform semantics.
A detached model can't establish the behavior of the production C# adapter.
Coyote's [controlled task scheduling](https://microsoft.github.io/coyote/concepts/tasks/overview/) alone doesn't control the adapter's native pipe operations.
The required substitute checks include the [transport cases](../../app/FindMyFiles.Tests/PipeConnectionTests.cs), [concurrent timeout case](../../app/FindMyFiles.Tests/PipeEngineClientAdditionalTests.cs), and [generation-isolation case](../../app/FindMyFiles.Tests/PipeEngineClientTests.cs).
The existing native service integration gate remains mandatory.
The checks exercise production source and preserve the existing mutation inventory and accepted source identities.
The coverage is finite and specific to the exercised schedules.
Reconsider source-bound concurrency verification when a maintained verifier covers the adapter's atomic, task, timer, and native pipe semantics, or when retirement gains a separate decision core.

The supervisor can't own the only cause because admitted writes need the same cause.
A client-wide lock would serialize independent generations and introduce another lock order.
Longer sleeps or retries can't establish the required cleanup order.

## Consequences

Concurrent retirement paths report one cause per connection generation.
The change preserves the public engine interface, pipe frames, stored formats, and native library name.
The cause occupies one nullable reference for the connection lifetime.
Native concurrency checks remain necessary alongside the deterministic Rust proofs.
