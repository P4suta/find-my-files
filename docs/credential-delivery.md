---
doppler_automation_app: release_please
---

# Credential delivery

Doppler synchronizes the release App credentials into the main-only release-please environment.
The [sync manifest](../automation/release-app-sync.toml) maps RELEASE_APP_CLIENT_ID and RELEASE_APP_PRIVATE_KEY to the existing automation config's GITHUB_APP_CLIENT_ID and GITHUB_APP_PRIVATE_KEY references.
The dedicated github_automation config contains those two App fields and Doppler's generated metadata.
GitHub stores credential copies, and protected main and environment policies replace Doppler's workflow and event claims for App access.
The shared App key retains authority across its installations.

The proposal, atomic lockfile commit, and API-only publication jobs read the synchronized fields.
Their inline validation rejects empty or unresolved values before the pinned token action runs.
Each pinned action restricts its installation token to `find-my-files` and the listed permissions.
The action revokes the token during cleanup.
The parsed workflow contract rejects unrelated secret consumers, different repositories, changed main guards, broader token permissions, and skipped token revocation.

Windows signing keeps its Doppler OIDC path through `find-my-files.sign_windows` and DOPPLER_SIGN_WINDOWS_IDENTITY_ID in the protected release environment.
Publication keeps GitHub OIDC for Sigstore attestations.
The existing signing approval and separate publication approval remain required.
Build, SBOM, mutation, collection, packaging, and approval jobs receive no App private key.

## Verification and recovery

The manual **Verify credential access** workflow defaults to App authentication from main.
It validates the two fields, creates a repository-scoped token with contents read access, and revokes the token during cleanup.
Selecting verify_signing also runs the existing signing credential and config-isolation checks behind the release environment's approval.
The workflow performs no signing, tagging, release creation, or publication.

Inspect field names and synchronization health without reading secret values.
Pause release-please during the reviewed cutover, run App verification from the merged main commit, confirm token revocation and skipped signing verification, then restore the proposal workflow.
Keep the existing automation identity, public identity variable, and source config for recovery through a reviewed revert PR.
Identity retirement requires a separate decision.

Keep credential values and native authentication state out of source, issues, and logs.
Rotation of the canonical App fields propagates through their Doppler references and synchronization.
See [the decision](adr/sync-release-app-credentials-to-the-protected-environment.md) and [release operations](RELEASING.md).
