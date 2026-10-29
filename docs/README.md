# Documentation

Early Windows x64 releases are available from the [GitHub Releases page](https://github.com/P4suta/find-my-files/releases).

The app searches file names and doesn't index file contents.

Unpack the Windows x64 release archive.
Run `FindMyFiles.exe` from the extracted root.
Enter a filename such as `report-2026.pdf` in the app's `Search by file name…` box.

Initial indexing reads the Master File Table.

Updates from the New Technology File System (NTFS) journal keep the index current.

The app omits files marked hidden or system, and files under hidden or system folders, by default.

A setting can include those files in search results.

The app indexes only fixed NTFS volumes.

The app doesn't support volumes that use the Resilient File System.

It also skips File Allocation Table and extended File Allocation Table volumes, removable media, and network drives.

The first run uses one User Account Control (UAC) prompt to register an on-demand service.

After setup, the app runs without elevation and connects through a named pipe restricted to authorized users.

## Building

Run `mise install` to install the pinned tools.

Run `just setup` to install the Git hooks.

Run `just doctor` to compare installed tools with the pins in `mise.toml`.

Run `just build` to build the Rust engine in release mode.

Run `just publish` to assemble the app and engine in `build/dist/FindMyFiles/`.
Start `FindMyFiles.exe` from the assembled root.

Run `just --list` to see the full task menu.

Run `just verify-push` before pushing.
The command runs the full `just verify` gate on Windows.
Other operating systems run `just verify-portable` for TOML formatting, spelling, and workflow checks.

Read the [security model](SECURITY.md) for service and pipe details.

Read the [design summary](SUMMARY.md), [technical research](RESEARCH.md), and [release guide](RELEASING.md) for project details.

Browse the published [design book](https://p4suta.github.io/find-my-files/book/) for architecture decisions.

Read the repository's [contribution guide](../CONTRIBUTING.md) before changing code.

The [product overview in English](https://p4suta.github.io/find-my-files/en/) and [product overview in Japanese](https://p4suta.github.io/find-my-files/) describe the app for users.
