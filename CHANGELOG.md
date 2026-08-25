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
- Bounded non-password accessibility values on macOS and Windows, with secure
  text fields redacted before they cross the native actor boundary.
- Structured request-wait and detached-execution latency events without command
  payloads or typed text.
- Bounded request identities, active sessions, and application-allowlist memory.
- Per-process artifact generations that are removed on graceful runtime
  teardown without deleting their host-owned root.
- Product integration contract for Nexus CLI/Skill consumption and independent
  CUA/Browser enablement.
- Bounded IPC connections and in-flight requests, plus an owner-and-SYSTEM-only
  Windows named-pipe DACL.
- Native Unix-socket and Windows named-pipe end-to-end CI smoke coverage.
- Bounded transport tokens with one shared CLI token-file decoder.
- `make smoke` for the native local IPC end-to-end check.
- Current Node 24 GitHub Actions and grouped weekly dependency updates.
