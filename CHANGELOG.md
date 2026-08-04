# Changelog

## [0.2.0](https://github.com/P4suta/find-my-files/compare/v0.1.1...v0.2.0) (2026-08-04)


### ⚠ BREAKING CHANGES

* complete service-backed release architecture ([#164](https://github.com/P4suta/find-my-files/issues/164))

### Features

* complete service-backed release architecture ([#164](https://github.com/P4suta/find-my-files/issues/164)) ([0f3d873](https://github.com/P4suta/find-my-files/commit/0f3d873fb20f642e4adedfdd4cdfbbf98e9e9684))
* **engine:** enable Control Flow Guard on all engine binaries ([#151](https://github.com/P4suta/find-my-files/issues/151)) ([fe83a66](https://github.com/P4suta/find-my-files/commit/fe83a660126caefed5ceb6a9ed734d19ed0ba15b))
* **engine:** keep overflow checks on in release for the boundary crates ([#153](https://github.com/P4suta/find-my-files/issues/153)) ([ff5814f](https://github.com/P4suta/find-my-files/commit/ff5814f4425af87d82dfd9b3a657c99a320a0df4))


### Bug Fixes

* align mutation shards with reviewed C# policy ([#184](https://github.com/P4suta/find-my-files/issues/184)) ([845f1ec](https://github.com/P4suta/find-my-files/commit/845f1ecae36667cc7c274b54fe1e1491f38b4f41))
* **app:** log successful pipe reconnects and localize the page-fetch failure notice ([#145](https://github.com/P4suta/find-my-files/issues/145)) ([56f2e1b](https://github.com/P4suta/find-my-files/commit/56f2e1bc53508291e2374e071a21b1b3afa6e43e))
* **ci:** accept optional Stryker status reason ([#180](https://github.com/P4suta/find-my-files/issues/180)) ([c29a347](https://github.com/P4suta/find-my-files/commit/c29a347971d17e1c5f435b9fc8a99add69a56780))
* **ci:** include contract binding in mutation input ([#179](https://github.com/P4suta/find-my-files/issues/179)) ([51dd052](https://github.com/P4suta/find-my-files/commit/51dd0523b651661e2d95101f1942bad0accc2364))
* **ci:** remove duplicate mutation test arg ([#178](https://github.com/P4suta/find-my-files/issues/178)) ([b9bcb4f](https://github.com/P4suta/find-my-files/commit/b9bcb4fc387576dcff28707ceec436ae3aac65fd))
* **ci:** shorten mutation work paths ([#176](https://github.com/P4suta/find-my-files/issues/176)) ([9f407a2](https://github.com/P4suta/find-my-files/commit/9f407a2d77f8373352199e4eafb0aa53e62e475a))
* **ci:** stabilize mutation baselines ([#177](https://github.com/P4suta/find-my-files/issues/177)) ([ffd851f](https://github.com/P4suta/find-my-files/commit/ffd851f2a5959f59c255f2cdce38f699466d60a4))
* **deps:** bump crossbeam-epoch to 0.9.20 (RUSTSEC-2026-0204) ([#154](https://github.com/P4suta/find-my-files/issues/154)) ([0b7bdb7](https://github.com/P4suta/find-my-files/commit/0b7bdb7c01ff18a97127d46e77c91d93c15af8a0))
* **engine:** bound USN tail reads so service stop cannot hang on a quiet volume ([#143](https://github.com/P4suta/find-my-files/issues/143)) ([e2c63ca](https://github.com/P4suta/find-my-files/commit/e2c63caa78fa5f98273f759c02160e2c9a58dc90))
* harden release gates and service recovery UX ([#172](https://github.com/P4suta/find-my-files/issues/172)) ([c8c3ce1](https://github.com/P4suta/find-my-files/commit/c8c3ce1ff1a8d666da1721daaaed44d13b392b86))
* harden release validation paths ([#174](https://github.com/P4suta/find-my-files/issues/174)) ([a98fc4b](https://github.com/P4suta/find-my-files/commit/a98fc4b6768a793a9eff9a2173f43247bd3919b4))
* honour space-form --data-dir and drain in-flight io-probe reads on error ([#144](https://github.com/P4suta/find-my-files/issues/144)) ([f8252e7](https://github.com/P4suta/find-my-files/commit/f8252e7a601bb8e0a9c5acc1f2feffa4386b0e43))
* **publish:** ship LICENSE and third-party notices in the bundle ([#147](https://github.com/P4suta/find-my-files/issues/147)) ([0cef8c9](https://github.com/P4suta/find-my-files/commit/0cef8c97c9bdc8b7b29904034bec4c65fc24a2e8))
* residual-check dotted extension drivers ([#186](https://github.com/P4suta/find-my-files/issues/186)) ([2b5e9ed](https://github.com/P4suta/find-my-files/commit/2b5e9ed957b6fa5a5b94b9a02e3b661e97b6c158))
* **service:** make the managed-tree walks handle-relative ([#171](https://github.com/P4suta/find-my-files/issues/171)) ([56b77e0](https://github.com/P4suta/find-my-files/commit/56b77e08352abb25a403dd95e00f89b3da541f82))
* ship self-contained bundle (embed .NET runtime, not framework-dependent) ([#138](https://github.com/P4suta/find-my-files/issues/138)) ([b550655](https://github.com/P4suta/find-my-files/commit/b550655b0f055d169693d6e3ec9e79717485ce09))
* **version:** align .dirty stamping across stampers and correct the README status ([#148](https://github.com/P4suta/find-my-files/issues/148)) ([e63841f](https://github.com/P4suta/find-my-files/commit/e63841f8095e345110909e685a10ed83ce118534))


### Code Refactoring

* move release signing shuffle + admin-test env into xtask ([#131](https://github.com/P4suta/find-my-files/issues/131)) ([ee9f7eb](https://github.com/P4suta/find-my-files/commit/ee9f7eb92ce7029716b90111080eb52ee09c5a71))
* **release:** remove the workflow_run indirection from the release path ([#170](https://github.com/P4suta/find-my-files/issues/170)) ([22eaa7c](https://github.com/P4suta/find-my-files/commit/22eaa7c9fd9d023f3f67b480d9b9849fb2927662))

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
