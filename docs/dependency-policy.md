# Dependency and licensing policy

Nexus Computer Use Runtime owns its protocol, runtime, transport, and native
driver behavior. It
does not vendor, wrap, download, or execute another Computer Use runtime.
Third-party code is limited to general Rust infrastructure and bindings to
public operating-system APIs.

## Admission rules

- Runtime dependencies must use permissive terms compatible with commercial
  redistribution: MIT, Apache-2.0, BSD-3-Clause, Zlib, 0BSD, Unlicense, or an
  equivalent approved license.
- GPL, AGPL, SSPL, source-available, non-commercial, and proprietary runtime
  dependencies are not accepted.
- Native executables, helper services, and copied implementation code require a
  separate review; adding a crate is not permission to copy its source into a
  Nexus-owned implementation.
- `Cargo.lock` is committed. CI and release builds use `--locked`, and each
  internal crate pins the matching Nexus Computer Use Runtime version as well
  as its local path.
- A release review must inspect the complete resolved dependency graph, update
  `THIRD_PARTY_NOTICES.md`, and retain the generated source archives and binary
  checksums as evidence.

## Current audit

The `0.1.0` lockfile contains no dependency with a missing license declaration
and no dependency that requires Nexus Computer Use Runtime to be distributed under a copyleft
license. Platform bindings are permissively licensed and call Apple or
Microsoft public APIs; they do not introduce a separately licensed Computer Use
engine.

This policy is an engineering release gate, not legal advice. Product
distribution, trademarks, signing identities, platform entitlements, and store
terms remain the responsibility of the embedding product.
