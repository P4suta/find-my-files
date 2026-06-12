# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** via
[GitHub Security Advisories](https://github.com/P4suta/find-my-files/security/advisories/new).
**Do not open a public issue for a vulnerability.**

We aim to acknowledge a report within a few days and to ship a fix or mitigation
as quickly as the severity warrants.

## Supported versions

find-my-files is pre-1.0; only the latest release receives security fixes.

| Version | Supported |
| ------- | --------- |
| latest  | ✅        |
| older   | ❌        |

## Scope

The full threat model and trust boundaries are documented in
[`docs/SECURITY.md`](../docs/SECURITY.md). Examples of in-scope reports:

- Bypassing the named-pipe access control (the pipe is same-user only)
- Privilege escalation via `fmf-service` (LocalSystem with stripped privileges)
- Disclosing file names a user should not see, beyond the documented accepted
  residual risk in `docs/SECURITY.md`

Out of scope: the residual risks explicitly accepted in `docs/SECURITY.md`.
