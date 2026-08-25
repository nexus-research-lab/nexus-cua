# Changelog

All notable changes to Nexus Computer Use Runtime will be documented in this
file.

## [Unreleased]

### Changed

- Corrected the Nexus integration boundary: the Agent Runtime owns reasoning
  and the agent loop, Nexus owns its CLI/Skill broker and managed sidecar
  installation, and optional CLI/Skill or MCP adapters remain above the core
  host contract.
- Session manifests now select short-lived trusted-host discovery references;
  session creation revalidates runtime epoch, process generation, and native
  executable identity before granting authority.
- Session and discovery expiry now use a nearest-deadline scheduler, including
  idle artifact deletion and an explicit graceful runtime shutdown path.
- Request reconciliation now retains completed results for a configurable
  10-minute default horizon and rejects capacity instead of evicting an
  unexpired result.
- Adopted Nexus Computer Use Runtime as the canonical project name while
  retaining `nexus-cua`, crate names, environment variables, and
  `nexus.cua.v1` as the technical domain namespace.
- Separated runtime session/reference projection from command orchestration and
  consolidated platform-neutral observation integrity helpers.
- Reframed the documentation around one Nexus-first core with independent
  ecosystem integration surfaces and an explicit delivery roadmap.

### Added

- Committed `nexus.cua.v1` request/response schemas, exhaustive compatibility
  fixtures, schema-drift CI, Markdown link checks, and a compatibility policy.
- Transport-authenticated application discovery with runtime-local opaque refs,
  public macOS code-signing/Windows Authenticode provenance summaries, and
  stable `stale_discovery` recovery.
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
  Computer Use/Browser enablement.
- Bounded IPC connections and in-flight requests, plus an owner-and-SYSTEM-only
  Windows named-pipe DACL.
- Native Unix-socket and Windows named-pipe end-to-end CI smoke coverage.
- Bounded transport tokens with one shared CLI token-file decoder.
- `make smoke` for the native local IPC end-to-end check.
- Current Node 24 GitHub Actions and grouped weekly dependency updates.
