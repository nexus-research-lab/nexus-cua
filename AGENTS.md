# AGENTS.md

## Purpose

`nexus-cua` is a model-neutral, cross-platform Computer Use runtime. Keep agent
reasoning, provider adapters, product preferences, chat identities, and Nexus
domain semantics out of this repository.

## Boundaries

- The public protocol is versioned and capability-negotiated.
- Drivers never infer user consent. Every mutation requires host-issued
  authority validated by the runtime.
- Applications, windows, sessions, observations, and elements use opaque refs;
  public callers do not supply platform process handles as authority.
- Screenshots and accessibility content are transient and must not be logged.
- Browser-specific DOM, history, network, CDP, and background-tab access are
  outside the core desktop contract.
- Platform drivers implement this repository's own contract. Do not copy an
  external Computer Use runtime or leak another project's schema into the
  public protocol.

## Development

- Read existing files before editing.
- Prefer small, cohesive changes and keep code and documentation in sync.
- Run `make check` before declaring a change complete.
- Use English commit messages with an emoji prefix.
- User-visible changes belong in `CHANGELOG.md` after the first release.

## Release

- Release artifacts are built from a pinned source revision.
- Publish checksums, an SBOM, and third-party notices with every release.
- Never download executable driver code or enable third-party self-update at
  runtime.
