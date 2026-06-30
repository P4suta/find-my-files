# Changelog

## [0.2.0](https://github.com/P4suta/find-my-files/compare/v0.1.1...v0.2.0) (2026-06-30)


### Features

* automate versioning with release-please + build channels ([#99](https://github.com/P4suta/find-my-files/issues/99)) ([cd00c07](https://github.com/P4suta/find-my-files/commit/cd00c0712c8c90ededf497ba8531230378a4171e))
* **cli:** second DevEx pass — completions, format consistency, drift-in-CI (ADR-0039) ([#115](https://github.com/P4suta/find-my-files/issues/115)) ([a9d6f08](https://github.com/P4suta/find-my-files/commit/a9d6f08c94f2bde662ca2638e0e6a8e86f54a739))
* **diagnostics:** logfmt structured logging with cross-process correlation ([#113](https://github.com/P4suta/find-my-files/issues/113)) ([7346c64](https://github.com/P4suta/find-my-files/commit/7346c64fa2177020554d94cf57d240f88e6c50f7))
* **dist:** surface build identity in downloaded artifacts (ADR-0038) ([#114](https://github.com/P4suta/find-my-files/issues/114)) ([3329453](https://github.com/P4suta/find-my-files/commit/3329453ec42d8ea6d296315f837a744bdbe0f257))
* working release-please bump (inherited workspace) + release safety gates ([#101](https://github.com/P4suta/find-my-files/issues/101)) ([d7f8ed3](https://github.com/P4suta/find-my-files/commit/d7f8ed3987a024e2c319f770fed64e776802ffe1))


### Bug Fixes

* **app:** connect through the pipe after onboarding instead of polling ([#107](https://github.com/P4suta/find-my-files/issues/107)) ([2c3f4cd](https://github.com/P4suta/find-my-files/commit/2c3f4cd0da5ce8a0eee81e59a87f4c7a43c982cb))
* **app:** re-resolve engine in-process after onboarding instead of relaunching ([#108](https://github.com/P4suta/find-my-files/issues/108)) ([c7c88d2](https://github.com/P4suta/find-my-files/commit/c7c88d2c5ddb9ec11a0915f4e4b9eb1a83637915))
* **ci:** make the release pipeline immutable-compatible (draft-first publish) ([#122](https://github.com/P4suta/find-my-files/issues/122)) ([da95521](https://github.com/P4suta/find-my-files/commit/da95521209fe11414ec7b39100b3da0419f45287))
* **ci:** only reinstate autorelease:pending on an OPEN release PR ([#124](https://github.com/P4suta/find-my-files/issues/124)) ([fd72bc7](https://github.com/P4suta/find-my-files/commit/fd72bc7248df83d8f1375d5f14570edbe68bb353))
* keep release-please changelog under engine/ (no '..' in changelog-path) ([#100](https://github.com/P4suta/find-my-files/issues/100)) ([6852da4](https://github.com/P4suta/find-my-files/commit/6852da44131b99c8794dc7824b7b80ae905eba9e))
* pin the first release to 0.1.0 via release-as ([#106](https://github.com/P4suta/find-my-files/issues/106)) ([cf27942](https://github.com/P4suta/find-my-files/commit/cf279420dea8166c4ca22bc73707eacfdbda9270))
* resolve release PR branch in bash and target 0.1.0 as the first release ([#105](https://github.com/P4suta/find-my-files/issues/105)) ([1155428](https://github.com/P4suta/find-my-files/commit/11554280fc09ee9de18678c20f6242c3a773ec9d))
* root the release-please package so version-file updaters resolve ([#103](https://github.com/P4suta/find-my-files/issues/103)) ([850a0ab](https://github.com/P4suta/find-my-files/commit/850a0abc7ded19b1cecb8013e7d2bd5a781edcaa))
* **service:** register GC scheduled task XML as UTF-16 for non-English Windows ([#118](https://github.com/P4suta/find-my-files/issues/118)) ([8d79d78](https://github.com/P4suta/find-my-files/commit/8d79d789b23b7e3fd0409f941ccfd292c17a8d52))

## [0.1.1](https://github.com/P4suta/find-my-files/compare/v0.1.0...v0.1.1) (2026-06-30)


### Bug Fixes

* **ci:** make the release pipeline immutable-compatible (draft-first publish) ([#122](https://github.com/P4suta/find-my-files/issues/122)) ([da95521](https://github.com/P4suta/find-my-files/commit/da95521209fe11414ec7b39100b3da0419f45287))
* **ci:** only reinstate autorelease:pending on an OPEN release PR ([#124](https://github.com/P4suta/find-my-files/issues/124)) ([fd72bc7](https://github.com/P4suta/find-my-files/commit/fd72bc7248df83d8f1375d5f14570edbe68bb353))

## 0.1.0 (2026-06-30)


### Features

* automate versioning with release-please + build channels ([#99](https://github.com/P4suta/find-my-files/issues/99)) ([cd00c07](https://github.com/P4suta/find-my-files/commit/cd00c0712c8c90ededf497ba8531230378a4171e))
* **cli:** second DevEx pass — completions, format consistency, drift-in-CI (ADR-0039) ([#115](https://github.com/P4suta/find-my-files/issues/115)) ([a9d6f08](https://github.com/P4suta/find-my-files/commit/a9d6f08c94f2bde662ca2638e0e6a8e86f54a739))
* **diagnostics:** logfmt structured logging with cross-process correlation ([#113](https://github.com/P4suta/find-my-files/issues/113)) ([7346c64](https://github.com/P4suta/find-my-files/commit/7346c64fa2177020554d94cf57d240f88e6c50f7))
* **dist:** surface build identity in downloaded artifacts (ADR-0038) ([#114](https://github.com/P4suta/find-my-files/issues/114)) ([3329453](https://github.com/P4suta/find-my-files/commit/3329453ec42d8ea6d296315f837a744bdbe0f257))
* working release-please bump (inherited workspace) + release safety gates ([#101](https://github.com/P4suta/find-my-files/issues/101)) ([d7f8ed3](https://github.com/P4suta/find-my-files/commit/d7f8ed3987a024e2c319f770fed64e776802ffe1))


### Bug Fixes

* **app:** connect through the pipe after onboarding instead of polling ([#107](https://github.com/P4suta/find-my-files/issues/107)) ([2c3f4cd](https://github.com/P4suta/find-my-files/commit/2c3f4cd0da5ce8a0eee81e59a87f4c7a43c982cb))
* **app:** re-resolve engine in-process after onboarding instead of relaunching ([#108](https://github.com/P4suta/find-my-files/issues/108)) ([c7c88d2](https://github.com/P4suta/find-my-files/commit/c7c88d2c5ddb9ec11a0915f4e4b9eb1a83637915))
* keep release-please changelog under engine/ (no '..' in changelog-path) ([#100](https://github.com/P4suta/find-my-files/issues/100)) ([6852da4](https://github.com/P4suta/find-my-files/commit/6852da44131b99c8794dc7824b7b80ae905eba9e))
* pin the first release to 0.1.0 via release-as ([#106](https://github.com/P4suta/find-my-files/issues/106)) ([cf27942](https://github.com/P4suta/find-my-files/commit/cf279420dea8166c4ca22bc73707eacfdbda9270))
* resolve release PR branch in bash and target 0.1.0 as the first release ([#105](https://github.com/P4suta/find-my-files/issues/105)) ([1155428](https://github.com/P4suta/find-my-files/commit/11554280fc09ee9de18678c20f6242c3a773ec9d))
* root the release-please package so version-file updaters resolve ([#103](https://github.com/P4suta/find-my-files/issues/103)) ([850a0ab](https://github.com/P4suta/find-my-files/commit/850a0abc7ded19b1cecb8013e7d2bd5a781edcaa))
* **service:** register GC scheduled task XML as UTF-16 for non-English Windows ([#118](https://github.com/P4suta/find-my-files/issues/118)) ([8d79d78](https://github.com/P4suta/find-my-files/commit/8d79d789b23b7e3fd0409f941ccfd292c17a8d52))

## Changelog

All notable changes to find-my-files are recorded here.

This file is maintained **automatically** by [release-please](https://github.com/googleapis/release-please)
from [Conventional Commits](https://www.conventionalcommits.org/) — do not edit it
by hand. A merged Release PR adds a new section and cuts the matching `vX.Y.Z`
tag. See [ADR-0035](docs/adr/0035-automated-versioning-with-release-please-and-build-channels.md).
