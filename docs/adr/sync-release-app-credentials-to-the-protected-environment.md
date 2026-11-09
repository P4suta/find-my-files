---
date: 2026-11-09
status: proposed
---

# Sync release App credentials to the protected environment

## Context

The release App needs credentials in main-only jobs.
Doppler's [identity limit](https://docs.doppler.com/docs/platform-limits) constrains fleet growth.
The release-please environment admits main and prevents administrator bypass.

## Decision

Use [native GitHub synchronization](https://docs.doppler.com/docs/github-actions) from a dedicated config referencing the existing automation fields.
Expose only two named App fields to guarded consumers with explicit repository permissions.
Keep signing OIDC, Sigstore OIDC, and both release approvals.
Manual verification defaults to read-only App authentication.

## Alternatives

Doppler OIDC avoids GitHub copies and retains workflow and event claims, but consumes identity capacity.
A service token adds another stored credential.

## Consequences

GitHub stores credential copies.
Protected main and environment policies replace Doppler claims for App access.
The shared App key retains installation-wide authority.
Reconsider when the App supports workload identity or the environment admits another ref.

## Confirmation

Require rejecting workflow fixtures, native project checks, synchronization health, and read-only authentication from reviewed main.
Confirm token revocation before restoring the paused proposal workflow.
Keep the existing identity until the owner approves retirement or recovery.
