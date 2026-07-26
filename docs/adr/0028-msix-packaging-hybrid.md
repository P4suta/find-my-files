# ADR-0028: Do not distribute an MSIX

Date: 2026-06-24 / Status: Rejected 2026-07-07

## Decision

Ship one signed, self-contained ZIP. Do not package the WinUI process while
leaving the LocalSystem service outside the package.

## Why

- The evaluated hybrid built, installed, launched, and searched, but had no
  supported unattended distribution path for the project’s signing identity.
- MSIX cannot own the service lifecycle without discarding the custom
  service-object DACL, privilege reduction, data-tree hardening, and GC model
  required by ADR-0017 and ADR-0027.
- Maintaining a second installation shape duplicated setup, path, update, and
  uninstall behavior without improving the product.

The abandoned implementation remains recoverable at
`archive/msix-attempt-2026-07`; it is not a supported surface.

## Reconsider only when

An official unattended signer supports the project identity and MSIX can
preserve the service security/lifecycle model without a parallel installer.
