# ADR-0041: nextest as the canonical Rust test runner

Date: 2026-07-25 / Status: Accepted. Supersedes only the cargo-nextest rejection in [ADR-0014](0014-build-tooling-rejections.md).

All Rust unit/integration tests use `cargo nextest run`; stable-Rust doctests remain the separate `cargo test --doc` gate because nextest cannot enumerate them. The two Cargo workspaces own separate `.config/nextest.toml` files, while the executable version is pinned once in `mise.toml`.

Retries are disabled and flaky passes fail. Slow tests are terminated after a bounded number of timeout periods, and every run has a global timeout. CI, lefthook, coverage, mutation testing, targeted recipes, and ignored admin tests share this executor.

ADR-0014 rejected nextest when a small pure suite showed no speed benefit. The release pass added a real overlapped-I/O lifecycle test and exposed an indefinitely stuck `cargo test` binary. Per-test attribution, process isolation, Windows Job Object termination, and JUnit evidence now outweigh the extra tool. This adoption is for bounded, diagnosable tests—not a speed claim. Raising global timeouts or enabling retries to hide one unstable test is rejected.
