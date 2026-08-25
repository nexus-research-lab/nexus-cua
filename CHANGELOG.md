# Changelog

All notable changes to Nexus CUA will be documented in this file.

## [Unreleased]

### Added

- Initial model-neutral Computer Use protocol and runtime architecture.
- Authenticated bounded local IPC, request idempotency, CLI diagnostics, and
  development Makefile workflows.
- Native actor, coordinate, observation coherence, and performance contracts
  for macOS and Windows.
- Native macOS `ScreenCaptureKit`/`AXUIElement`/`CGEvent` and Windows
  `Windows.Graphics.Capture`/UI Automation/`SendInput` drivers.
- Bounded accessibility traversal, exact window-generation identities,
  visual stale-state guards, and private transient PNG artifacts.
- Concurrent request joining and post-timeout reconciliation under stable
  idempotency keys.
- A single automatic observation-coherence retry, bounded PNG worker pool, and
  per-session screenshot retention limit.
- Minimum-Rust macOS and Windows CI gates.
- Attested universal macOS and Windows x64 GitHub release packages with SHA-256
  checksums.
- Publishable version-pinned Rust crates and an exact-window v1 schema without
  dormant desktop or background-input grants.
- Stable executable-path fallback identities for macOS applications without a
  bundle identifier.
